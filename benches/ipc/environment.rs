#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub digest: String,
}

pub fn digest(bytes: &[u8]) -> String {
    format!(
        "{:016x}",
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    )
}

pub fn output(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} failed"));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

pub struct Capture {
    pub context: crate::study::Context,
    pub command: String,
    pub started: String,
}

pub fn metadata(environment: &Snapshot) -> Result<Capture, String> {
    use crate::study::{Compiler, Context, Host, Source};
    use std::time::{SystemTime, UNIX_EPOCH};

    let rustc = output("rustc", &["--version", "--verbose"])?;
    let target = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc omitted host target")?
        .to_owned();
    let state = output("git", &["status", "--porcelain"])?;
    let diff = output("git", &["diff", "HEAD", "--no-ext-diff", "--binary"])?;
    Ok(Capture {
        context: Context {
            source: Source {
                revision: output("git", &["rev-parse", "HEAD"])?,
                dirty: !state.is_empty(),
                diff: digest(format!("{state}\n{diff}").as_bytes()),
            },
            compiler: Compiler {
                target,
                rustc: rustc.replace(['\t', '\n', '\r'], " "),
            },
            host: Host {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                description: environment.text.clone(),
                environment: environment.digest.clone(),
            },
        },
        command: std::env::args().collect::<Vec<_>>().join(" "),
        started: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis()
            .to_string(),
    })
}

#[cfg(windows)]
fn platform() -> Result<String, String> {
    output(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "$ErrorActionPreference='Stop';\
             $OutputEncoding=[Console]::OutputEncoding=[Text.UTF8Encoding]::new();\
             $aff=(Get-Process -Id $PID).ProcessorAffinity.ToInt64().ToString('x');\
             $power=((& powercfg /getactivescheme 2>$null) -join ' ').Trim();\
             if(-not $power){$power='unavailable'};\
             'cpu='+$env:PROCESSOR_IDENTIFIER+';affinity='+$aff+\
             ';page='+[Environment]::SystemPageSize+';power='+$power+\
             ';kernel='+[Environment]::OSVersion.VersionString+';native=windows'",
        ],
    )
}

#[cfg(unix)]
fn platform() -> Result<String, String> {
    output(
        "sh",
        &[
            "-lc",
            "cpu=$(awk -F: '/model name|Hardware/{sub(/^ /,\"\",$2);print $2;exit}' /proc/cpuinfo);\
             aff=$(awk '/Cpus_allowed_list/{print $2}' /proc/self/status);\
             page=$(getconf PAGESIZE);\
             power=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unavailable);\
             kernel=$(uname -srvmo | tr ';' ',');\
             if grep -qi microsoft /proc/version; then native=wsl; else native=linux; fi;\
             printf 'cpu=%s;affinity=%s;page=%s;power=%s;kernel=%s;native=%s' \"$cpu\" \"$aff\" \"$page\" \"$power\" \"$kernel\" \"$native\"",
        ],
    )
}

pub fn capture() -> Result<Snapshot, String> {
    let text = format!(
        "os={};arch={};logical={};{};physical=unavailable;smt=unavailable;thermal=unavailable;background=unavailable",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism().map_or(0, usize::from),
        platform()?
    );
    Ok(Snapshot {
        digest: digest(text.as_bytes()),
        text,
    })
}

pub fn admit(expected: &str) -> Result<Snapshot, String> {
    let snapshot = capture()?;
    (snapshot.digest == expected)
        .then_some(snapshot)
        .ok_or_else(|| "environment mismatch".into())
}
