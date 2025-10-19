mod state;
use crate::state::State;
use std::{io::{stdout, Write}, net::ToSocketAddrs, time::{Duration, Instant}};
use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Parser};
use humantime::parse_duration;
use crossterm::{cursor, terminal, ExecutableCommand};
use tokio::{pin, time};
use surge_ping::{Client, Config, PingIdentifier, PingSequence};

#[derive(Parser, Debug)]
#[command(name = "pngr", version)]
struct Args {
    /// Hostname or IP to ping
    #[arg(long, default_value = "localhost", help = "Target hostname or IP address")]
    host: String,

    /// Interval between pings (250ms, 1s)
    #[arg(long, default_value = "1s")]
    interval: String,

    /// Per-ping timeout (1s)
    #[arg(long, default_value = "1s")]
    timeout: String,

    /// Number of pings to send (0 = infinite until Ctrl-C)
    #[arg(long, default_value_t = 0, help = "The number of pings.  0 = infinite.  Default is 0.")]
    count: u64,

    /// Payload size in bytes
    #[arg(long, default_value_t = 32)]
    size: usize,

    /// Sparkline width (rolling history)
    #[arg(long, default_value_t = 50)]
    history: usize,

    /// Force subprocess fallback instead of raw sockets
    #[arg(long, action = ArgAction::SetTrue)]
    no_raw: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let interval = parse_duration(&args.interval).context("Invalid --interval")?;
    let timeout = parse_duration(&args.timeout).context("Invalid --timeout")?;

    // Resolve the target, we only need the IP address
    let target_address = resolve_host(&args.host).context("Failed to resolve host address")?;

    // Terminal setup
    let _terminal_guard = TerminalGuard::enter().context("failed to initialize terminal")?;

    println!("📡 pngr → {}  (interval {}, timeout {}, size {}B)",
             args.host, args.interval, args.timeout, args.size);
    println!("Press Ctrl-C to quit.\n");

    // Try raw ICMP first, unless otherwise directed by args.
    let raw_ok = !args.no_raw;
    let mut state = State::new(args.history);

    let run_future = async {
        if raw_ok {
            run_raw_loop(&args, target_address, interval, timeout, &mut state).await
        } else {
            run_fallback_loop(&args, interval, &mut state).await
        }
    };
    pin!(run_future);

    let res = tokio::select! {
        res = run_future => res,
        ctrl = tokio::signal::ctrl_c() => {
            match ctrl {
                Ok(_) => {
                    println!("\nInterrupted by Ctrl-C.");
                    Ok(())
                }
                Err(e) => Err(anyhow!("Failed to listen for Ctrl-C: {e}")),
            }
        }
    };

    if let Err(e) = res {
        eprintln!("error: {e:?}");
        return Err(e);
    }

    Ok(())
}


/// Resolves a hostname or IP string into an `IpAddr` using the system DNS resolver.
/// Returns the first address found or an error if none are available.
fn resolve_host(host: &str) -> Result<std::net::IpAddr> {
    let mut addrs = (host, 0).to_socket_addrs()?;
    let ip = addrs.find_map(|socket_addr| Some(socket_addr.ip()))
        .ok_or_else(|| anyhow!("Failed to resolve host address"))?;
    Ok(ip)
}


/// Sends ICMP echo requests directly through raw sockets using `surge-ping`.
async fn run_raw_loop(args: &Args, ip: std::net::IpAddr, interval: Duration, timeout: Duration, state: &mut State) -> Result<()> {
    let client = Client::new(&Config::default()).context("create surge-ping client")?;
    let mut pinger = client.pinger(ip, PingIdentifier(random_id())).await;
    pinger.timeout(timeout);

    let mut seq = 0u16;

    let mut ticker = time::interval(interval);
    loop {
        ticker.tick().await;
        let payload = vec![0xab; args.size];
        seq = seq.wrapping_add(1);
        let reply = pinger.ping(PingSequence(seq), &payload).await;
        let rtt = reply.ok().map(|(_, elapsed)| elapsed);
        state.push(rtt);
        render(&args.host, state);
        if args.count > 0 && state.sent >= args.count { break; }
    }

    Ok(())
}

/// Executes the cross-platform fallback by shelling out to the system `ping` utility per interval.
async fn run_fallback_loop(args: &Args, interval: Duration, state: &mut State) -> Result<()> {
    // Simple cross-platform fallback by spawning system ping with 1 probe each loop
    // (Linux: ping -c 1 -W 1 host; macOS: ping -c 1 -W 1000 host; Windows: ping -n 1 -w 1000 host)
    #[cfg(target_os = "windows")]
    let cmd_tpl = |host: &str| {
        vec![
            "ping".to_owned(),
            "-n".into(),
            "1".into(),
            "-w".into(),
            "1000".into(),
            host.to_owned(),
        ]
    };
    #[cfg(target_os = "linux")]
    let cmd_tpl = |host: &str| {
        vec![
            "ping".to_owned(),
            "-c".into(),
            "1".into(),
            "-W".into(),
            "1".into(),
            host.to_owned(),
        ]
    };
    #[cfg(target_os = "macos")]
    let cmd_tpl = |host: &str| {
        vec![
            "ping".to_owned(),
            "-c".into(),
            "1".into(),
            "-W".into(),
            "1000".into(),
            host.to_owned(),
        ]
    };

    let mut ticker = time::interval(interval);
    loop {
        ticker.tick().await;
        let cmd_args = cmd_tpl(&args.host);
        let t0 = Instant::now();
        let out = tokio::process::Command::new(&cmd_args[0])
            .args(&cmd_args[1..])
            .output()
            .await;
        let rtt = match out {
            Ok(o) if o.status.success() => {
                // very cheap parse: if success, use wall time as approximation
                Some(t0.elapsed())
            }
            _ => None,
        };
        state.push(rtt);
        render(&args.host, state);
        if args.count > 0 && state.sent >= args.count { break; }
    }
    Ok(())
}


/// Builds the ASCII sparkline representation for the given target and current state.
fn render(target: &str, state: &State) {

    let mut buffer = String::new();
    let last_n = state.samples.iter()
        .cycle()
        .skip(state.index.saturating_sub(state.samples.len()) % state.samples.len())
        .take(state.samples.len());
    let mut max_ms:f64 = 0.0;
    for s in last_n.clone() {
        if let Some(d) = s {
            max_ms = max_ms.max(d.as_secs_f64() * 1000.0);
        }
    }
    let max_ms = if max_ms <= 0.0 { 1.0 } else { max_ms };

    for s in last_n {
        match s {
            None => buffer.push('.'),
            Some(d) => {
                let ms = d.as_secs_f64() * 1000.0;
                let level = ((ms / max_ms) * 5.0).clamp(0.0, 5.0) as u8;
                let ch = match level {
                    0 => '▁',
                    1 => '▂',
                    2 => '▃',
                    3 => '▅',
                    4 => '▆',
                    _ => '▇'
                };
                buffer.push(ch);
            }
        }
    }

    let avg = state.avg().map(|d| format!("{:.1}", d.as_secs_f64() * 1000.0)).unwrap_or("-".into());
    let min = state.min.map(|d| format!("{:.1}", d.as_secs_f64() * 1000.0)).unwrap_or("-".into());
    let max = state.max.map(|d| format!("{:.1}", d.as_secs_f64() * 1000.0)).unwrap_or("-".into());
    let jit = state.jitter().map(|j| format!("{:.1}", j)).unwrap_or("-".into());
    let loss = state.loss_percentage();

    print!("\r{:<20} {}  sent:{} recv:{} loss:{:.1}%  min:{}ms  avg:{}ms  max:{}ms  jitter:{} ms    ",
           target, buffer, state.sent, state.received, loss, min, avg, max, jit);
    let _ = std::io::stdout().flush();
}

/// Produces a pseudo-random 16-bit identifier by hashing the current time.
fn random_id() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos();
    (nanos ^ 0xBEEF) as u16
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut out = stdout();
        out.execute(terminal::EnterAlternateScreen)?;
        out.execute(cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = out.execute(cursor::Show);
        let _ = out.execute(terminal::LeaveAlternateScreen);
        let _ = out.flush();
        println!();
    }
}
