//! Optional tuning file `/Library/Printers/LBP2900/lbp2900.conf` (`key = value` lines).

use std::time::Duration;

use capt::EngineOptions;

pub const CONFIG_PATH: &str = "/Library/Printers/LBP2900/lbp2900.conf";

pub fn apply(opts: &mut EngineOptions, text: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warnings.push(format!("line {}: expected key = value", n + 1));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let flag = || matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
        let num = || v.parse::<u64>().ok();
        match k {
            "send_job_info" => opts.send_job_info = flag(),
            "clear_before_job" => opts.clear_before_job = flag(),
            "start_print" => opts.start_print = flag(),
            "go_online" => opts.go_online = flag(),
            "go_online_arg" => opts.go_online_arg = num().unwrap_or(0) as u16,
            "reserve_arg" => opts.reserve_arg = num().unwrap_or(0) as u16,
            "blind_session" => opts.blind_session = num().unwrap_or(0) as u16,
            "page_timeout" => opts.page_timeout = Duration::from_secs(num().unwrap_or(120)),
            "poll_interval_ms" => opts.poll_interval = Duration::from_millis(num().unwrap_or(500)),
            "read_timeout_ms" => opts.read_timeout = Duration::from_millis(num().unwrap_or(5000)),
            _ => warnings.push(format!("line {}: unknown key {k}", n + 1)),
        }
    }
    warnings
}

pub fn load(opts: &mut EngineOptions) -> Vec<String> {
    match std::fs::read_to_string(CONFIG_PATH) {
        Ok(text) => apply(opts, &text),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_keys() {
        let mut o = EngineOptions::default();
        let w = apply(&mut o, "go_online = false # test\nreserve_arg=3\nbogus=1\n");
        assert!(!o.go_online);
        assert_eq!(o.reserve_arg, 3);
        assert_eq!(w.len(), 1);
    }
}
