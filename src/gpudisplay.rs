//! `headless="virtual-gpu"`: headless Xorg display pinned to a single GPU, matching `gpudisplay.ts` / `gpudisplay.py`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};
use crate::virtdisplay::assert_linux;
use crate::warnings::LeakWarning;

pub const GPU_ENV_VAR: &str = "CAMOUFOX_GPU";
pub const GPU_WAIT_S: u64 = 30;
pub const GPU_WAIT_ENV_VAR: &str = "CAMOUFOX_GPU_WAIT_S";
pub const GPU_SCREEN: &str = "1920x1080x24";
pub const SYSTEM_CONFIG_DIR: &str = "/etc/X11";
pub const XWRAPPER_CONFIG: &str = "/etc/X11/Xwrapper.config";
pub const XORG_WRAPPERS: [&str; 2] = ["/usr/lib/xorg/Xorg.wrap", "/usr/libexec/Xorg.wrap"];
pub const SERVER_FLAGS: [&str; 6] = [
    "-terminate",
    "2",
    "-nolisten",
    "tcp",
    "-sharevts",
    "-novtswitch",
];

pub const PROBE_PAGE: &str = "data:text/html,<canvas id=c width=32 height=32></canvas><script>\
const gl=c.getContext('webgl2')||c.getContext('webgl');\
function f(){if(gl){gl.clear(gl.COLOR_BUFFER_BIT);gl.flush();}requestAnimationFrame(f);}f();\
</script>";

#[cfg(unix)]
const X11_SETUP: [u8; 12] = [0x6c, 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0];

pub fn gpu_prefs() -> Map<String, Value> {
    let mut m = Map::new();
    for key in [
        "gfx.webrender.all",
        "layers.acceleration.force-enabled",
        "gfx.x11-egl.enabled",
        "gfx.canvas.accelerated",
        "gfx.canvas.accelerated.force-enabled",
    ] {
        m.insert(key.to_owned(), Value::Bool(true));
    }
    m
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDevice {
    pub pci_address: String,
    pub render_node: String,
    pub card_node: Option<String>,
    pub vendor_id: String,
    pub device_id: String,
    pub kernel_driver: String,
    pub nvidia_node: Option<String>,
}

pub fn gpu_vendor(gpu: &GpuDevice) -> &str {
    match gpu.vendor_id.as_str() {
        "0x10de" => "NVIDIA",
        "0x1002" => "AMD",
        "0x8086" => "Intel",
        other => other,
    }
}

pub fn bus_id(gpu: &GpuDevice) -> String {
    let mut parts = gpu.pci_address.split(':');
    let _domain = parts.next();
    let bus = parts.next().unwrap_or("0");
    let dev_fn = parts.next().unwrap_or("0.0");
    let mut df = dev_fn.split('.');
    let dev = u32::from_str_radix(df.next().unwrap_or("0"), 16).unwrap_or(0);
    let func = u32::from_str_radix(df.next().unwrap_or("0"), 16).unwrap_or(0);
    let bus_num = u32::from_str_radix(bus, 16).unwrap_or(0);
    format!("PCI:{bus_num}:{dev}:{func}")
}

pub fn xorg_driver(gpu: &GpuDevice) -> &'static str {
    if gpu.kernel_driver == "nvidia" {
        "nvidia"
    } else {
        "modesetting"
    }
}

pub fn drew_on_gpu(fdinfo: &str) -> bool {
    let re = Regex::new(r"(?m)^drm-(?:engine-[\w-]+:\s*(\d+) ns|cycles-[\w-]+:\s*(\d+))$")
        .expect("regex");
    for caps in re.captures_iter(fdinfo) {
        let val = caps
            .get(1)
            .or_else(|| caps.get(2))
            .and_then(|m| m.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        if val > 0 {
            return true;
        }
    }
    false
}

pub fn drm_nodes(gpu: &GpuDevice) -> Vec<&str> {
    let mut out = vec![gpu.render_node.as_str()];
    if let Some(ref c) = gpu.card_node {
        out.push(c.as_str());
    }
    out
}

pub fn describe_gpu(gpu: &GpuDevice) -> String {
    format!(
        "{} {} [{}:{}] driver={} {}",
        gpu.pci_address,
        gpu_vendor(gpu),
        gpu.vendor_id,
        gpu.device_id,
        gpu.kernel_driver,
        gpu.render_node
    )
}

fn read_trimmed(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn nvidia_node(root: &Path, pci_address: &str) -> Option<String> {
    let info = read_trimmed(
        &root
            .join("proc/driver/nvidia/gpus")
            .join(pci_address)
            .join("information"),
    );
    let re = Regex::new(r"(?m)^Device Minor:\s*(\d+)").expect("regex");
    let caps = re.captures(&info)?;
    Some(format!("/dev/nvidia{}", &caps[1]))
}

pub fn enumerate_gpus_at(root: &Path) -> Vec<GpuDevice> {
    let drm = root.join("sys/class/drm");
    let mut cards: HashMap<PathBuf, String> = HashMap::new();
    let card_re = Regex::new(r"^card\d+$").expect("regex");
    if let Ok(entries) = fs::read_dir(&drm) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if card_re.is_match(&name) {
                if let Ok(real) = fs::canonicalize(drm.join(&name).join("device")) {
                    cards.insert(real, format!("/dev/dri/{name}"));
                }
            }
        }
    }

    let pci_re = Regex::new(r"^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-9a-f]$").expect("regex");
    let mut gpus = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("dev/dri")) {
        for entry in entries.flatten() {
            let node = entry.file_name().to_string_lossy().into_owned();
            if !node.starts_with("renderD") {
                continue;
            }
            let device = drm.join(&node).join("device");
            let Ok(real) = fs::canonicalize(&device) else {
                continue;
            };
            let pci = real
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            if !pci_re.is_match(&pci) {
                continue;
            }
            let mut driver = String::new();
            for line in read_trimmed(&device.join("uevent")).lines() {
                if let Some(rest) = line.strip_prefix("DRIVER=") {
                    driver = rest.to_owned();
                }
            }
            let nv_node = if driver == "nvidia" {
                nvidia_node(root, &pci)
            } else {
                None
            };
            gpus.push(GpuDevice {
                pci_address: pci,
                render_node: format!("/dev/dri/{node}"),
                card_node: cards.get(&real).cloned(),
                vendor_id: read_trimmed(&device.join("vendor")).to_ascii_lowercase(),
                device_id: read_trimmed(&device.join("device")).to_ascii_lowercase(),
                kernel_driver: driver,
                nvidia_node: nv_node,
            });
        }
    }
    gpus.sort_by(|a, b| a.pci_address.cmp(&b.pci_address));
    gpus
}

pub fn enumerate_gpus() -> Vec<GpuDevice> {
    enumerate_gpus_at(Path::new("/"))
}

fn gpu_matches(index: usize, gpu: &GpuDevice, selector: &str) -> bool {
    if !selector.is_empty() && selector.bytes().all(|b| b.is_ascii_digit()) {
        return selector.parse::<usize>().ok() == Some(index);
    }
    let ids = format!("{}:{}", gpu.vendor_id, gpu.device_id).replace("0x", "");
    let wanted = selector.replace("0x", "");
    let mut names = vec![
        gpu.render_node.clone(),
        gpu_vendor(gpu).to_owned(),
        ids,
    ];
    if let Some(ref c) = gpu.card_node {
        names.push(c.clone());
    }
    let base_eq = |s: &str| {
        let lower = s.to_ascii_lowercase();
        let base = Path::new(s)
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or(s)
            .to_ascii_lowercase();
        lower == wanted || base == wanted
    };
    names.iter().any(|n| base_eq(n)) || gpu.pci_address.contains(selector)
}

pub fn select_gpu(selector: Option<&str>, gpus: Option<&[GpuDevice]>) -> Result<GpuDevice> {
    let owned;
    let list = match gpus {
        Some(g) => g,
        None => {
            owned = enumerate_gpus();
            &owned
        }
    };
    if list.is_empty() {
        return Err(CamoufoxError::NoGpuFound(
            "virtual-gpu: no GPU render node with a PCI address. Is a GPU present, is its driver \
             loaded, and are /dev/dri and /sys visible (--device /dev/dri in a container)?"
                .into(),
        ));
    }
    let env_val = std::env::var(GPU_ENV_VAR).unwrap_or_default();
    let wanted = selector.unwrap_or(&env_val).trim().to_ascii_lowercase();
    let format_list = |subset: &[&GpuDevice]| -> String {
        subset
            .iter()
            .enumerate()
            .map(|(i, g)| format!("  [{i}] {}", describe_gpu(g)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if wanted.is_empty() {
        if list.len() == 1 {
            return Ok(list[0].clone());
        }
        let all: Vec<&GpuDevice> = list.iter().collect();
        return Err(CamoufoxError::AmbiguousGpuSelection(format!(
            "virtual-gpu: {} GPUs found. Name one with gpu= or {GPU_ENV_VAR}, preferably by PCI address:\n{}",
            list.len(),
            format_list(&all)
        )));
    }
    let matched: Vec<&GpuDevice> = list
        .iter()
        .enumerate()
        .filter(|(i, g)| gpu_matches(*i, g, &wanted))
        .map(|(_, g)| g)
        .collect();
    if matched.is_empty() {
        let all: Vec<&GpuDevice> = list.iter().collect();
        return Err(CamoufoxError::GpuNotFound(format!(
            "virtual-gpu: no GPU matches '{wanted}'. Available:\n{}",
            format_list(&all)
        )));
    }
    if matched.len() > 1 {
        return Err(CamoufoxError::AmbiguousGpuSelection(format!(
            "virtual-gpu: '{wanted}' matches {} GPUs:\n{}",
            matched.len(),
            format_list(&matched)
        )));
    }
    Ok(matched[0].clone())
}

pub fn xorg_config(gpu: &GpuDevice, screen: Option<&str>) -> String {
    let scr = screen.unwrap_or(GPU_SCREEN);
    let mut parts = scr.split('x');
    let width = parts.next().unwrap_or("1920");
    let height = parts.next().unwrap_or("1080");
    let depth = parts.next().unwrap_or("24");
    let driver = xorg_driver(gpu);
    let offscreen = if driver == "nvidia" {
        "\tOption      \"UseDisplayDevice\" \"None\"\n"
    } else {
        ""
    };
    format!(
        "# Generated by camoufox for headless=\"virtual-gpu\".\n\
# GPU: {pci} {vendor} [{vid}:{did}] driver={drv}\n\
Section \"ServerFlags\"\n\
\tOption      \"AutoAddDevices\" \"false\"\n\
\tOption      \"AutoAddGPU\"     \"false\"\n\
\tOption      \"DontVTSwitch\"   \"true\"\n\
\tOption      \"AllowMouseOpenFail\" \"true\"\n\
EndSection\n\n\
Section \"Device\"\n\
\tIdentifier  \"camoufox-gpu\"\n\
\tDriver      \"{driver}\"\n\
\tBusID       \"{busid}\"\n\
{offscreen}\tOption      \"AllowEmptyInitialConfiguration\" \"true\"\n\
EndSection\n\n\
Section \"Monitor\"\n\
\tIdentifier  \"camoufox-monitor\"\n\
\tHorizSync   5.0 - 1000.0\n\
\tVertRefresh 5.0 - 200.0\n\
EndSection\n\n\
Section \"Screen\"\n\
\tIdentifier  \"camoufox-screen\"\n\
\tDevice      \"camoufox-gpu\"\n\
\tMonitor     \"camoufox-monitor\"\n\
\tDefaultDepth {depth}\n\
\tSubSection \"Display\"\n\
\t\tDepth   {depth}\n\
\t\tVirtual {width} {height}\n\
\tEndSubSection\n\
EndSection\n\n\
Section \"ServerLayout\"\n\
\tIdentifier  \"camoufox-layout\"\n\
\tScreen      \"camoufox-screen\"\n\
EndSection\n",
        pci = gpu.pci_address,
        vendor = gpu_vendor(gpu),
        vid = gpu.vendor_id,
        did = gpu.device_id,
        drv = gpu.kernel_driver,
        busid = bus_id(gpu),
    )
}

pub fn config_name(gpu: &GpuDevice) -> String {
    format!(
        "camoufox-gpu-{}.conf",
        gpu.pci_address.replace([':', '.'], "_")
    )
}

pub fn setup_command(gpu: &GpuDevice) -> String {
    format!("camoufox gpu-setup {} | sudo sh", gpu.pci_address)
}

pub fn setup_script(gpu: &GpuDevice) -> String {
    let config_path = Path::new(SYSTEM_CONFIG_DIR).join(config_name(gpu));
    let xorg_conf = xorg_config(gpu, None);
    let nvidia_check = if xorg_driver(gpu) == "nvidia" {
        "\nif ! find /usr/lib /usr/lib64 -name nvidia_drv.so 2>/dev/null | grep -q .; then\n\
         \techo 'camoufox gpu-setup: no nvidia_drv.so found. Install the NVIDIA driver Xorg module.' >&2\n\
         fi\n"
    } else {
        ""
    };
    format!(
        "#!/bin/sh\n\
# Generated by camoufox gpu-setup for headless=\"virtual-gpu\".\n\
# GPU: {desc}\n\
# Run once as root: {cmd}\n\
set -e\n\
if [ \"$(id -u)\" -ne 0 ]; then\n\
\techo 'camoufox gpu-setup: run this as root (pipe it to sudo sh).' >&2\n\
\texit 1\n\
fi\n\n\
if command -v apt-get >/dev/null 2>&1; then\n\
\tdpkg -s xserver-xorg-core xserver-xorg-legacy >/dev/null 2>&1 \\\n\
\t\t|| DEBIAN_FRONTEND=noninteractive apt-get install -y xserver-xorg-core xserver-xorg-legacy\n\
elif ! command -v Xorg >/dev/null 2>&1; then\n\
\techo 'camoufox gpu-setup: install the Xorg server and Xorg.wrap, then run this again.' >&2\n\
\texit 1\n\
fi\n\
{nvidia_check}\n\
touch {XWRAPPER_CONFIG}\n\
sed -i -e '/^[[:space:]]*allowed_users[[:space:]]*=/d' -e '/^[[:space:]]*needs_root_rights[[:space:]]*=/d' {XWRAPPER_CONFIG}\n\
printf 'allowed_users=anybody\\nneeds_root_rights=yes\\n' >> {XWRAPPER_CONFIG}\n\n\
cat > {cfg} <<'CAMOUFOX_XORG_CONF'\n\
{xorg_conf}CAMOUFOX_XORG_CONF\n\n\
echo 'headless=\"virtual-gpu\" is set up for {pci}.'\n",
        desc = describe_gpu(gpu),
        cmd = setup_command(gpu),
        cfg = config_path.display(),
        pci = gpu.pci_address,
    )
}

#[derive(Debug)]
pub struct GpuDisplay {
    pub gpu: GpuDevice,
    pub debug: bool,
    pub require_gpu: bool,
    pub display: Option<u32>,
    proc: Option<Child>,
    temp_dir: Option<PathBuf>,
    #[cfg(unix)]
    client: Option<std::os::unix::net::UnixStream>,
}

impl GpuDisplay {
    pub fn new(gpu: GpuDevice, debug: bool, require_gpu: bool) -> Self {
        Self {
            gpu,
            debug,
            require_gpu,
            display: None,
            proc: None,
            temp_dir: None,
            #[cfg(unix)]
            client: None,
        }
    }

    pub fn get(&mut self) -> Result<String> {
        assert_linux()?;
        if let Some(d) = self.display {
            return Ok(format!(":{d}"));
        }
        let xorg_out = Command::new("which")
            .arg("Xorg")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CamoufoxError::CannotFindXorg(format!(
                    "headless=\"virtual-gpu\" needs the Xorg server. Set this machine up with:\n  {}",
                    setup_command(&self.gpu)
                ))
            })?;

        let conf_name = config_name(&self.gpu);
        let installed_conf = Path::new(SYSTEM_CONFIG_DIR).join(&conf_name);
        let mut args = vec![
            "-displayfd".to_owned(),
            "1".to_owned(),
            "-config".to_owned(),
            if installed_conf.exists() {
                conf_name
            } else {
                let dir = std::env::temp_dir().join(format!("camoufox-xorg-{}", std::process::id()));
                let _ = fs::create_dir_all(&dir);
                let conf_file = dir.join("xorg.conf");
                fs::write(&conf_file, xorg_config(&self.gpu, None))?;
                self.temp_dir = Some(dir);
                conf_file.to_string_lossy().into_owned()
            },
        ];
        args.extend(SERVER_FLAGS.iter().map(|s| (*s).to_owned()));

        let mut child = Command::new(&xorg_out)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if self.debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .map_err(|e| CamoufoxError::CannotExecuteXorg(format!("Could not start Xorg: {e}")))?;

        let mut stdout = child.stdout.take().ok_or_else(|| {
            CamoufoxError::CannotExecuteXorg("Could not open Xorg -displayfd pipe".into())
        })?;
        let mut buf = String::new();
        let mut byte = [0u8; 1];
        let deadline = Instant::now() + Duration::from_secs(30);
        use std::io::Read;
        while Instant::now() < deadline {
            match stdout.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0] as char);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let parsed: u32 = buf.trim().parse().map_err(|_| {
            let _ = child.kill();
            CamoufoxError::CannotExecuteXorg(format!(
                "Xorg did not report a display (got '{buf}'). See {}",
                setup_command(&self.gpu)
            ))
        })?;
        self.proc = Some(child);
        self.display = Some(parsed);

        std::thread::sleep(Duration::from_millis(1000));
        #[cfg(unix)]
        {
            if let Ok(mut stream) =
                std::os::unix::net::UnixStream::connect(format!("/tmp/.X11-unix/X{parsed}"))
            {
                let _ = stream.write_all(&X11_SETUP);
                self.client = Some(stream);
            }
        }
        Ok(format!(":{parsed}"))
    }

    pub fn uses_gpu(&self) -> bool {
        let Some(display) = self.display else {
            return false;
        };
        let needle = format!("\0DISPLAY=:{display}\0");
        let nodes = drm_nodes(&self.gpu);
        let Ok(procs) = fs::read_dir("/proc") else {
            return false;
        };
        for entry in procs.flatten() {
            let pid_str = entry.file_name().to_string_lossy().into_owned();
            if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(environ) = fs::read(format!("/proc/{pid_str}/environ")) else {
                continue;
            };
            let env_str = String::from_utf8_lossy(&environ);
            if !format!("\0{env_str}").contains(&needle) {
                continue;
            }
            let Ok(fds) = fs::read_dir(format!("/proc/{pid_str}/fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let fd_name = fd.file_name().to_string_lossy().into_owned();
                let Ok(target) = fs::read_link(fd.path()) else {
                    continue;
                };
                let target_str = target.to_string_lossy();
                if self.gpu.nvidia_node.as_deref() == Some(target_str.as_ref()) {
                    return true;
                }
                if nodes.contains(&target_str.as_ref()) {
                    if let Ok(fdinfo) = fs::read_to_string(format!("/proc/{pid_str}/fdinfo/{fd_name}")) {
                        if drew_on_gpu(&fdinfo) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    pub async fn wait_for_gpu(&self) -> bool {
        let wait_s = std::env::var(GPU_WAIT_ENV_VAR)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(GPU_WAIT_S);
        let deadline = Instant::now() + Duration::from_secs(wait_s);
        while Instant::now() < deadline {
            if self.uses_gpu() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        false
    }

    pub fn check_gpu(&self, active: bool) -> Result<()> {
        if active {
            return Ok(());
        }
        if !self.require_gpu {
            LeakWarning::warn("gpu_software_rendering", None);
            return Ok(());
        }
        Err(CamoufoxError::GpuSoftwareFallback(format!(
            "The browser on :{} never rendered on {}, so it renders in software.",
            self.display.unwrap_or(0),
            describe_gpu(&self.gpu)
        )))
    }

    pub fn kill(&mut self) {
        #[cfg(unix)]
        {
            self.client.take();
        }
        if let Some(mut child) = self.proc.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(dir) = self.temp_dir.take() {
            let _ = fs::remove_dir_all(dir);
        }
        self.display = None;
    }
}

impl Drop for GpuDisplay {
    fn drop(&mut self) {
        self.kill();
    }
}

pub async fn prove_gpu(
    browser: &playwright_rs::protocol::Browser,
    display: &GpuDisplay,
) -> Result<()> {
    let context = browser.new_context().await?;
    let page = context.new_page().await?;
    let res = page.goto(PROBE_PAGE, None).await;
    let active = if res.is_ok() {
        display.wait_for_gpu().await
    } else {
        false
    };
    let _ = page.close().await;
    let _ = context.close().await;
    res?;
    display.check_gpu(active)
}
