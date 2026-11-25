//! CPython / orjson / MT19937 compatibility helpers for bit-identical seed reproducibility.

use serde_json::{Map, Value};

/// Python `str` code-point comparison (UTF-8 lexicographic order equals Unicode scalar order).
pub fn compare_py_str(a: &str, b: &str) -> std::cmp::Ordering {
    a.cmp(b)
}

/// Formats `f64` matching CPython / orjson float output (`1.0`, `-0.0`, etc.).
pub fn format_orjson_float(x: f64) -> String {
    if !x.is_finite() {
        return "null".to_owned();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let abs = x.abs();
    if (1e-5..1e16).contains(&abs) && x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        serde_json::to_string(&x).unwrap_or_else(|_| "0.0".to_owned())
    }
}

/// Compact sorted-key JSON serialization matching `orjson.dumps(..., OPT_SORT_KEYS)`.
pub fn orjson_dumps(value: &Value, sort_keys: bool) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                format_orjson_float(f)
            } else {
                n.to_string()
            }
        }
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(|v| orjson_dumps(v, sort_keys)).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        orjson_dumps(v, sort_keys)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// Python `str(value)` over JSON values.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                format_orjson_float(f)
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        other => orjson_dumps(other, false),
    }
}

/// Python `repr(value)` for diagnostics and error messages.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        other => py_str(other),
    }
}

/// Python `type(value).__name__`.
pub fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Whether `value` is a Python `int` (in Python, `bool` is a subclass of `int`).
pub fn is_py_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64() || value.is_boolean()
}

/// Numeric conversion matching Python's `int`/`bool`/`float`.
pub fn num_f64(value: &Value) -> Option<f64> {
    if let Some(b) = value.as_bool() {
        Some(if b { 1.0 } else { 0.0 })
    } else {
        value.as_f64()
    }
}

/// `json.dumps(value, ensure_ascii=True, separators=(',', ':'))`.
pub fn py_json_dumps_ascii(value: &Value) -> String {
    let compact = orjson_dumps(value, false);
    let mut out = String::with_capacity(compact.len());
    for ch in compact.chars() {
        let code = ch as u32;
        if (0x7f..=0xffff).contains(&code) {
            out.push_str(&format!("\\u{code:04x}"));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Python truthiness (`bool(value)`).
pub fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// CPython `random.Random` Mersenne Twister (MT19937) matching Python & `python-random`.
#[derive(Debug, Clone)]
pub struct PyRandom {
    mt: [u32; 624],
    index: usize,
}

impl PyRandom {
    pub fn new(seed: Option<u64>) -> Self {
        match seed {
            Some(s) => Self::from_seed(s),
            None => Self::from_entropy(),
        }
    }

    pub fn from_seed(seed: u64) -> Self {
        let mut r = Self {
            mt: [0; 624],
            index: 624,
        };
        r.seed_u64(seed);
        r
    }

    pub fn from_entropy() -> Self {
        let seed = myuri_rs::generate_random_seed();
        Self::from_seed(u64::from(seed))
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..624 {
            let prev = self.mt[i - 1];
            self.mt[i] = 1_812_433_253_u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        self.index = 624;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19_650_218);
        let mut i = 1usize;
        let mut j = 0usize;
        let k_max = 624.max(key.len());
        for _ in 0..k_max {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i]
                ^ 1_664_525_u32.wrapping_mul(prev ^ (prev >> 30)))
            .wrapping_add(key[j])
            .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                self.mt[0] = self.mt[623];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i]
                ^ 1_566_083_941_u32.wrapping_mul(prev ^ (prev >> 30)))
            .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                self.mt[0] = self.mt[623];
                i = 1;
            }
        }
        self.mt[0] = 0x8000_0000;
    }

    pub fn seed_u64(&mut self, n: u64) {
        if n == 0 {
            self.init_by_array(&[0]);
            return;
        }
        let lo = (n & 0xffff_ffff) as u32;
        let hi = (n >> 32) as u32;
        if hi == 0 {
            self.init_by_array(&[lo]);
        } else {
            self.init_by_array(&[lo, hi]);
        }
    }

    pub fn genrand_uint32(&mut self) -> u32 {
        const MAG01: [u32; 2] = [0, 0x9908_b0df];
        if self.index >= 624 {
            for kk in 0..227 {
                let y = (self.mt[kk] & 0x8000_0000) | (self.mt[kk + 1] & 0x7fff_ffff);
                self.mt[kk] = self.mt[kk + 397] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            }
            for kk in 227..623 {
                let y = (self.mt[kk] & 0x8000_0000) | (self.mt[kk + 1] & 0x7fff_ffff);
                self.mt[kk] = self.mt[kk - 227] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            }
            let y = (self.mt[623] & 0x8000_0000) | (self.mt[0] & 0x7fff_ffff);
            self.mt[623] = self.mt[396] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            self.index = 0;
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// Python `Random.random()` (53-bit float in `[0.0, 1.0)`).
    pub fn random(&mut self) -> f64 {
        let a = u64::from(self.genrand_uint32() >> 5);
        let b = u64::from(self.genrand_uint32() >> 6);
        ((a * 67_108_864 + b) as f64) * (1.0 / 9_007_199_254_740_992.0)
    }

    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.random()
    }

    pub fn randbelow(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        let k = (usize::BITS - (n - 1).leading_zeros()) as usize;
        loop {
            let mut r = 0usize;
            let mut bits = k;
            let mut shift = 0usize;
            while bits > 32 {
                r |= (self.genrand_uint32() as usize) << shift;
                shift += 32;
                bits -= 32;
            }
            let top = (self.genrand_uint32() >> (32 - bits)) as usize;
            r |= top << shift;
            if r < n {
                return r;
            }
        }
    }

    pub fn randint(&mut self, a: i64, b: i64) -> i64 {
        a + (self.randbelow((b - a + 1) as usize) as i64)
    }

    pub fn choice<'a, T>(&mut self, seq: &'a [T]) -> Option<&'a T> {
        if seq.is_empty() {
            return None;
        }
        let idx = self.randbelow(seq.len());
        Some(&seq[idx])
    }

    pub fn choices<'a, T>(&mut self, pop: &'a [T], weights: &[f64]) -> Option<&'a T> {
        if pop.is_empty() || pop.len() != weights.len() {
            return None;
        }
        let mut cum = Vec::with_capacity(weights.len());
        let mut total = 0.0;
        for &w in weights {
            total += w;
            cum.push(total);
        }
        if total <= 0.0 {
            return self.choice(pop);
        }
        let x = self.random() * total;
        let idx = cum.partition_point(|&c| c <= x).min(pop.len() - 1);
        Some(&pop[idx])
    }

    pub fn shuffle<T>(&mut self, x: &mut [T]) {
        for i in (1..x.len()).rev() {
            let j = self.randbelow(i + 1);
            x.swap(i, j);
        }
    }

    pub fn sample<T: Clone>(&mut self, population: &[T], k: usize) -> Vec<T> {
        let n = population.len();
        if k >= n {
            let mut copy = population.to_vec();
            self.shuffle(&mut copy);
            return copy;
        }
        let mut pool = population.to_vec();
        let mut out = Vec::with_capacity(k);
        for i in 0..k {
            let j = self.randbelow(n - i);
            out.push(pool[j].clone());
            pool[j] = pool[n - i - 1].clone();
        }
        out
    }

    /// Python `random.Random.gauss(mu, sigma)`.
    pub fn gauss(&mut self, mu: f64, sigma: f64) -> f64 {
        let u1 = self.random();
        let u2 = self.random();
        let z = (-2.0 * (1.0 - u1).ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        mu + z * sigma
    }
}

/// CRC32 matching `zlib.crc32`.
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Neumaier-compensated sum matching CPython 3.12+ `sum()` over floats.
pub fn py_sum_floats(values: &[f64]) -> f64 {
    let mut f = 0.0_f64;
    let mut c = 0.0_f64;
    for (i, &x) in values.iter().enumerate() {
        if i == 0 {
            f = x;
            continue;
        }
        let t = f + x;
        if f.abs() >= x.abs() {
            c += f - t + x;
        } else {
            c += x - t + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    f
}

/// Utility converting a sorted string-keyed map into a `serde_json::Map`.
pub fn value_map(pairs: impl IntoIterator<Item = (String, Value)>) -> Map<String, Value> {
    pairs.into_iter().collect()
}
