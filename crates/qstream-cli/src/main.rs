use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use qstream_core::{capture, hex, monitor, record, token, Endian, Record};
use qstream_transport::{connect_wait, read_for, read_until_idle, Stream, Target};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "qstream",
    version,
    about = "Client for the Adreno qprofiler (QSTREAM) socket"
)]
struct Cli {
    /// TCP target, used when --unix is absent
    #[arg(long, global = true)]
    tcp: Option<String>,

    /// Unix target: @name for abstract, path otherwise
    #[arg(long, global = true)]
    unix: Option<String>,

    #[arg(long, global = true, default_value_t = 800)]
    connect_ms: u64,

    /// Keep retrying the connection for this long
    #[arg(long, global = true, default_value_t = 0)]
    wait_ms: u64,

    #[arg(long, global = true, default_value_t = 1000)]
    idle_ms: u64,

    #[arg(long, global = true, default_value_t = 1_048_576)]
    max_bytes: usize,

    /// Append raw replies to a file
    #[arg(long, global = true)]
    save: Option<PathBuf>,

    /// adb serial used by perfetto commands
    #[arg(long, global = true)]
    serial: Option<String>,

    /// Accept an empty reply or zero records as success
    #[arg(long, global = true)]
    allow_empty: bool,

    /// Print JSON for probe, scope, and monitor
    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true, default_value = "le", value_parser = parse_endian)]
    endian: Endian,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send register and dump the reply
    Probe,
    /// Start gpu scope and dump the version reply
    Version,
    /// Send raw hex frames
    Send {
        #[arg(long = "hex", required = true)]
        frames: Vec<String>,
        #[arg(long, default_value_t = 0)]
        gap_ms: u64,
        #[arg(long)]
        no_read: bool,
    },
    /// Connect and only read
    Listen,
    /// Send capture start
    Start,
    /// Send capture stop and read the reply for two seconds
    Stop,
    /// Request a capture of the next presented frame
    CaptureFrame {
        /// VkDevice pointer printed by the hosting app, e.g. 0xb4000079...
        #[arg(long)]
        device: Option<String>,
        /// Raw pointer halves for scripts: --arg low,high
        #[arg(long = "arg", value_delimiter = ',')]
        args: Vec<u32>,
    },
    /// Run a capture session and collect the stream
    Capture {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// Ask for the next frame instead of starting immediately
        #[arg(long)]
        frame: bool,
        /// VkDevice pointer used by --frame
        #[arg(long)]
        device: Option<String>,
        /// Write one row per captured frame
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Capture a Perfetto trace (needs the layer in direct load mode)
    Perfetto {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        #[arg(long, default_value = "qstream-trace.pb")]
        out: PathBuf,
        /// Push this config instead of the built-in gpu.renderstages one
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Start and stream fps for a few seconds
    Fps {
        #[arg(long, default_value_t = 2)]
        seconds: u64,
    },
    /// Start gpu scope and stream submit timing records; leaves the scope running
    Scope {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// Stop gpu scope
    ScopeOff,
    /// Set the frame rate period
    FrameRate {
        #[arg(long)]
        value: u32,
    },
    /// Set the frame delimiter flags
    Delimiters {
        #[arg(long)]
        value: u32,
    },
    /// List, add, or remove gpu scope counter pairs
    Counters {
        #[command(subcommand)]
        cmd: CounterCmd,
    },
    /// Start gpu scope in monitor mode and decode counter reports
    Monitor {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// GSL block id, also called group
        #[arg(long)]
        group: Option<u32>,
        /// Counter index inside the block
        #[arg(long)]
        counter: Option<u32>,
        /// Remove all curated pairs before adding the requested pair
        #[arg(long)]
        reset: bool,
        /// Write one row per monitor report, including samples
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// Decode a saved raw stream
    Decode { file: PathBuf },
    /// Print the known token table
    Tokens,
}

#[derive(Subcommand)]
enum CounterCmd {
    /// Print known blocks and common counters
    List {
        /// Only show entries for this GSL block
        #[arg(long)]
        group: Option<u32>,
    },
    /// Add a counter pair to every gpu scope; existing pairs are kept
    Add {
        #[arg(long)]
        group: u32,
        #[arg(long)]
        counter: u32,
        /// Remove all curated pairs before adding
        #[arg(long)]
        reset: bool,
    },
    /// Remove a counter pair from every gpu scope
    Remove {
        #[arg(long)]
        group: u32,
        #[arg(long)]
        counter: u32,
    },
}

fn parse_endian(s: &str) -> Result<Endian, String> {
    s.parse()
}

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplyMode {
    Required,
    Optional,
}

struct Reply {
    data: Vec<u8>,
    truncated: bool,
}

fn ensure_nonempty(what: &str, count: usize, allow_empty: bool) -> Result<()> {
    if count == 0 && !allow_empty {
        anyhow::bail!("{what}: no data; pass --allow-empty to accept");
    }
    Ok(())
}

fn capture_needs_stop(frame: bool) -> bool {
    !frame
}

fn dedup_pairs(pairs: &mut Vec<(u32, u32)>) {
    let mut seen = HashSet::new();
    pairs.retain(|pair| seen.insert(*pair));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CounterPlan {
    removes: Vec<(u32, u32)>,
    add: Option<(u32, u32)>,
    reset: bool,
}

const STOCK_PAIRS: &[(u32, u32)] = &[(0, 0), (27, 0)];

fn plan_counter_add(group: u32, counter: u32, reset: bool) -> CounterPlan {
    let mut removes = Vec::new();
    if reset {
        removes.extend(COUNTERS.iter().map(|&(g, c, _)| (g, c)));
    }
    removes.push((group, counter));
    dedup_pairs(&mut removes);
    CounterPlan {
        removes,
        add: Some((group, counter)),
        reset,
    }
}

fn plan_monitor(group: Option<u32>, counter: Option<u32>, reset: bool) -> Result<CounterPlan> {
    let pair = match (group, counter) {
        (Some(g), Some(c)) => Some((g, c)),
        (None, None) => None,
        _ => anyhow::bail!("--group and --counter must be used together"),
    };
    let Some((g, c)) = pair else {
        return Ok(CounterPlan {
            removes: Vec::new(),
            add: None,
            reset,
        });
    };

    let mut removes = Vec::new();
    if reset {
        removes.extend(COUNTERS.iter().map(|&(rg, rc, _)| (rg, rc)));
    } else {
        removes.extend_from_slice(STOCK_PAIRS);
    }
    removes.push((g, c));
    dedup_pairs(&mut removes);
    Ok(CounterPlan {
        removes,
        add: Some((g, c)),
        reset,
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.json
        && !matches!(
            &cli.cmd,
            Cmd::Probe | Cmd::Scope { .. } | Cmd::Monitor { .. }
        )
    {
        anyhow::bail!("--json is supported only by probe, scope, and monitor");
    }

    match &cli.cmd {
        Cmd::Tokens => {
            print_tokens();
            return Ok(());
        }
        Cmd::Counters {
            cmd: CounterCmd::List { group },
        } => {
            print_counters(*group);
            return Ok(());
        }
        Cmd::Perfetto {
            seconds,
            out,
            config,
        } => {
            run_perfetto(*seconds, out, config.as_ref(), cli.serial.as_deref())?;
            return Ok(());
        }
        Cmd::Decode { file } => {
            let data = std::fs::read(file).with_context(|| format!("read {:?}", file))?;
            ensure_nonempty("decode", data.len(), cli.allow_empty)?;
            if data.len() <= 4096 {
                print!("{}", hex::dump(&data));
            } else {
                println!("file bytes={}", data.len());
            }
            let (recs, used) = record::parse_lenient(&data, cli.endian);
            show_records(&recs, cli.endian);
            if used < data.len() {
                println!("tail {} bytes not framed", data.len() - used);
            }
            return Ok(());
        }
        _ => {}
    }

    let target = match (&cli.unix, &cli.tcp) {
        (Some(name), _) => Target::Unix(name.clone()),
        (None, Some(addr)) => Target::Tcp(addr.clone()),
        (None, None) => Target::Tcp("127.0.0.1:20829".to_string()),
    };

    let mut stream = connect_wait(
        &target,
        ms(cli.connect_ms),
        ms(cli.idle_ms),
        ms(cli.wait_ms),
    )
    .with_context(|| format!("connect {:?}", target))?;

    match &cli.cmd {
        Cmd::Probe => {
            send_cmd(
                &mut stream,
                &token::request(token::CMD_REGISTER, cli.endian),
                &cli,
            )?;
            let reply = read(&mut stream, &cli, ReplyMode::Required)?;
            if cli.json {
                print_json(&probe_json(&reply.data, reply.truncated, cli.endian))?;
            }
        }
        Cmd::Version => {
            send(&mut stream, &token::request_scope(0, 0, 0, cli.endian))?;
            let read_result = read_for(
                stream.as_mut(),
                Duration::from_secs(1),
                Duration::from_millis(200),
                cli.max_bytes,
            );
            let stop_result = stop_scope(&mut stream, &cli);
            let (data, truncated) = read_result.context("version read")?;
            stop_result?;
            finish_reply(data, truncated, &cli, ReplyMode::Required)?;
        }
        Cmd::Send {
            frames,
            gap_ms,
            no_read,
        } => {
            for frame in frames {
                let bytes = hex::parse(frame).with_context(|| format!("parse hex {frame:?}"))?;
                send(&mut stream, &bytes)?;
                if !no_read {
                    read_window(
                        stream.as_mut(),
                        &cli,
                        ms(cli.idle_ms),
                        ms(100),
                        "send",
                        ReplyMode::Required,
                    )?;
                }
                if *gap_ms > 0 {
                    std::thread::sleep(ms(*gap_ms));
                }
            }
            if *no_read {
                read_window(
                    stream.as_mut(),
                    &cli,
                    ms(cli.idle_ms),
                    ms(100),
                    "send",
                    ReplyMode::Required,
                )?;
            }
        }
        Cmd::Listen => {
            read(&mut stream, &cli, ReplyMode::Required)?;
        }
        Cmd::Start => {
            send(
                &mut stream,
                &token::request(token::CMD_CAPTURE_START, cli.endian),
            )?;
            read_window(
                stream.as_mut(),
                &cli,
                ms(500),
                ms(200),
                "start",
                ReplyMode::Required,
            )?;
        }
        Cmd::Stop => {
            send(
                &mut stream,
                &token::request(token::CMD_CAPTURE_STOP, cli.endian),
            )?;
            read_window(
                stream.as_mut(),
                &cli,
                Duration::from_secs(2),
                Duration::from_millis(500),
                "stop",
                ReplyMode::Required,
            )?;
        }
        Cmd::CaptureFrame { device, args } => {
            let dev = match device {
                Some(text) => parse_ptr(text)?,
                None => {
                    let lo = args.first().copied().unwrap_or(0) as u64;
                    let hi = args.get(1).copied().unwrap_or(0) as u64;
                    lo | (hi << 32)
                }
            };
            if dev == 0 {
                println!("warning: device pointer is 0, the layer may ignore the request");
            }
            send(
                &mut stream,
                &token::request_device(token::CMD_CAPTURE_FRAME, dev, cli.endian),
            )?;
            read_window(
                stream.as_mut(),
                &cli,
                ms(cli.idle_ms),
                ms(200),
                "capture-frame",
                ReplyMode::Required,
            )?;
        }
        Cmd::Capture {
            seconds,
            frame,
            device,
            csv,
        } => {
            let dev = match device {
                Some(text) => parse_ptr(text)?,
                None => 0,
            };
            run_capture(&mut stream, &cli, *seconds, *frame, dev, csv.as_ref())?;
        }
        Cmd::Fps { seconds } => {
            send(
                &mut stream,
                &token::request_u32(token::CMD_FPS, 1, cli.endian),
            )?;
            let read_result = read_for(
                stream.as_mut(),
                Duration::from_secs(*seconds),
                Duration::from_millis(200),
                cli.max_bytes,
            );
            let stop_result = stop_fps(&mut stream, &cli);
            let (data, truncated) = read_result.context("fps read")?;
            stop_result?;
            if let Some(path) = &cli.save {
                save(path, &data)?;
            }
            let (recs, used) = record::parse_lenient(&data, cli.endian);
            let vals: Vec<f32> = recs
                .iter()
                .filter_map(|r| record::fps_value(r, cli.endian))
                .collect();
            println!("fps samples={} truncated={truncated}", vals.len());
            if used < data.len() {
                println!("fps tail={} bytes not framed", data.len() - used);
            }
            if !vals.is_empty() {
                let min = vals.iter().copied().fold(f32::INFINITY, f32::min);
                let max = vals.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let avg = vals.iter().sum::<f32>() / vals.len() as f32;
                println!(
                    "fps min={min:.2} avg={avg:.2} max={max:.2} last={:.2}",
                    vals[vals.len() - 1]
                );
            }
            ensure_nonempty("fps", vals.len(), cli.allow_empty)?;
        }
        Cmd::Scope { seconds } => {
            send_cmd(
                &mut stream,
                &token::request_scope(0, 0, 0, cli.endian),
                &cli,
            )?;
            let (data, truncated) = read_for(
                stream.as_mut(),
                Duration::from_secs(*seconds),
                Duration::from_millis(300),
                cli.max_bytes,
            )
            .context("scope read")?;
            if let Some(path) = &cli.save {
                save(path, &data)?;
            }
            let (recs, used) = record::parse_lenient(&data, cli.endian);
            let timing = recs
                .iter()
                .filter(|r| r.kind == record::KIND_SUBMIT_TIMING)
                .count();
            let submits = recs
                .iter()
                .filter(|r| r.kind == record::KIND_SUBMIT_INFO)
                .count();
            let timings: Vec<_> = recs
                .iter()
                .filter_map(|r| {
                    let tok = capture::StateToken {
                        pid: r.arg0,
                        kind: r.kind,
                        payload: r.payload,
                    };
                    capture::submit_timing(&tok, cli.endian)
                })
                .collect();
            if cli.json {
                print_json(&scope_json(
                    data.len(),
                    truncated,
                    recs.len(),
                    timing,
                    submits,
                    data.len().saturating_sub(used),
                    &timings,
                ))?;
            } else {
                println!(
                    "scope bytes={} records={} timing={timing} submit_info={submits} truncated={truncated}",
                    data.len(),
                    recs.len()
                );
                if used < data.len() {
                    println!("scope tail={} bytes not framed", data.len() - used);
                }
            }
            ensure_nonempty("scope", timings.len(), cli.allow_empty)?;
        }
        Cmd::FrameRate { value } => {
            send(
                &mut stream,
                &token::request_u32(token::CMD_FRAME_RATE, *value, cli.endian),
            )?;
            read_window(
                stream.as_mut(),
                &cli,
                ms(300),
                ms(100),
                "frame-rate",
                ReplyMode::Optional,
            )?;
        }
        Cmd::Delimiters { value } => {
            send(
                &mut stream,
                &token::request_u32(token::CMD_FRAME_DELIMITERS, *value, cli.endian),
            )?;
            read_window(
                stream.as_mut(),
                &cli,
                ms(300),
                ms(100),
                "delimiters",
                ReplyMode::Optional,
            )?;
        }
        Cmd::ScopeOff => {
            send(
                &mut stream,
                &token::request(token::CMD_SCOPE_STOP, cli.endian),
            )?;
            read_window(
                stream.as_mut(),
                &cli,
                ms(300),
                ms(100),
                "scope-off",
                ReplyMode::Optional,
            )?;
        }
        Cmd::Counters { cmd } => run_counters(&mut stream, &cli, cmd)?,
        Cmd::Monitor {
            seconds,
            group,
            counter,
            reset,
            csv,
        } => run_monitor(
            &mut stream,
            &cli,
            *seconds,
            *group,
            *counter,
            *reset,
            csv.as_ref(),
        )?,
        Cmd::Tokens | Cmd::Decode { .. } | Cmd::Perfetto { .. } => unreachable!(),
    }

    Ok(())
}

fn run_capture(
    stream: &mut Box<dyn Stream>,
    cli: &Cli,
    seconds: u64,
    frame: bool,
    device: u64,
    csv: Option<&PathBuf>,
) -> Result<()> {
    drain(stream, cli, 300);

    if frame {
        if device == 0 {
            println!("warning: --frame without --device will not arm a specific device");
        }
        send(
            stream,
            &token::request_device(token::CMD_CAPTURE_FRAME, device, cli.endian),
        )?;
    } else {
        send(
            stream,
            &token::request(token::CMD_CAPTURE_START, cli.endian),
        )?;
    }

    let mut all: Vec<u8> = Vec::new();
    let mut truncated = false;

    let head_result = read_for(
        stream.as_mut(),
        Duration::from_secs(seconds),
        Duration::from_millis(200),
        cli.max_bytes,
    );
    let stop_result = if capture_needs_stop(frame) {
        write_frame(stream, &token::request(token::CMD_CAPTURE_STOP, cli.endian))
    } else {
        Ok(())
    };
    let (head, cut) = head_result.context("capture read")?;
    stop_result?;
    all.extend_from_slice(&head);
    truncated |= cut;

    if capture_needs_stop(frame) {
        let (tail, cut) = read_for(
            stream.as_mut(),
            Duration::from_secs(2),
            Duration::from_millis(500),
            cli.max_bytes,
        )
        .context("capture tail read")?;
        all.extend_from_slice(&tail);
        truncated |= cut;
    }

    if let Some(path) = &cli.save {
        save(path, &all)?;
    }

    println!("capture bytes={} truncated={truncated}", all.len());
    if all.is_empty() {
        ensure_nonempty("capture", 0, cli.allow_empty)?;
        println!("capture returned no data");
        return Ok(());
    }
    let preview = &all[..all.len().min(256)];
    print!("{}", hex::dump(preview));

    let (recs, used) = record::parse_lenient(&all, cli.endian);
    if used < all.len() {
        println!("capture tail={} bytes not framed", all.len() - used);
    }
    ensure_nonempty("capture records", recs.len(), cli.allow_empty)?;
    let mut counts: HashMap<u32, (usize, usize)> = HashMap::new();
    for rec in &recs {
        let entry = counts.entry(rec.kind).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += rec.payload.len();
    }
    println!("capture records={}", recs.len());
    let mut kinds: Vec<_> = counts.into_iter().collect();
    kinds.sort_by_key(|(kind, _)| *kind);
    for (kind, (n, bytes)) in kinds {
        println!(
            "  kind=0x{kind:08X} ({}) n={n} payload={bytes}",
            record::kind_name(kind)
        );
    }

    let mut markers = Vec::new();
    for rec in &recs {
        if rec.kind == record::KIND_FRAME_START || rec.kind == record::KIND_FRAME_END {
            markers.push((rec.index, word(rec.payload, 4, cli.endian)));
        }
    }
    if let (Some((start, index)), Some((end, _))) = (markers.first(), markers.last()) {
        println!(
            "capture frame marker={index} markers={} start={start} end={end}",
            markers.len()
        );
    }

    let mut frames = Vec::new();
    for rec in &recs {
        if rec.kind == record::KIND_SUBMIT_TIMING {
            if let (Some((start, _)), Some((end, _))) = (markers.first(), markers.last()) {
                if rec.index > *start && rec.index < *end {
                    let tok = capture::StateToken {
                        pid: rec.arg0,
                        kind: rec.kind,
                        payload: rec.payload,
                    };
                    if let Some(ft) = capture::submit_timing(&tok, cli.endian) {
                        frames.push(ft);
                    }
                }
            }
        }
        if rec.kind != record::KIND_FRAME_BATCH {
            continue;
        }
        match capture::parse_tokens(rec.payload, cli.endian) {
            Ok(tokens) => {
                let mut batch_timing = 0usize;
                for tok in &tokens {
                    if let Some(ft) = capture::submit_timing(tok, cli.endian) {
                        frames.push(ft);
                        batch_timing += 1;
                    }
                }
                println!("  batch tokens={} timing={batch_timing}", tokens.len());
            }
            Err(err) => println!("  batch parse failed: {err}"),
        }
    }

    if !frames.is_empty() {
        let first = frames.first().unwrap().frame_index;
        let last = frames.last().unwrap().frame_index;
        let durs: Vec<u64> = frames
            .iter()
            .map(|f| f.end_us.saturating_sub(f.start_us))
            .collect();
        let min = durs.iter().copied().min().unwrap_or(0);
        let max = durs.iter().copied().max().unwrap_or(0);
        let avg = durs.iter().sum::<u64>() / durs.len() as u64;
        println!(
            "capture submits={} index={first}..{last} dur_us min={min} avg={avg} max={max}",
            frames.len()
        );
        println!(
            "capture gpu_ts first=0x{:016X} last=0x{:016X}",
            frames.first().unwrap().gpu_ts,
            frames.last().unwrap().gpu_ts
        );
    }

    if let Some(path) = csv {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open {path:?}"))?;
        writeln!(
            file,
            "frame_index,pid,start_us,end_us,dur_us,gpu_ts,frame_clock,cpu_clock,gpu_ts2,gpu_ts3"
        )?;
        for f in &frames {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{},{},{}",
                f.frame_index,
                f.pid,
                f.start_us,
                f.end_us,
                f.end_us.saturating_sub(f.start_us),
                f.gpu_ts,
                f.frame_clock,
                f.cpu_clock,
                f.gpu_ts2,
                f.gpu_ts3
            )?;
        }
        println!("capture csv submits={} -> {path:?}", frames.len());
    }

    Ok(())
}

fn drain(stream: &mut Box<dyn Stream>, cli: &Cli, wait_ms: u64) {
    let _ = read_for(
        stream.as_mut(),
        Duration::from_millis(wait_ms),
        Duration::from_millis(50),
        cli.max_bytes,
    );
}

fn stop_scope(stream: &mut Box<dyn Stream>, cli: &Cli) -> Result<()> {
    write_frame(stream, &token::request(token::CMD_SCOPE_STOP, cli.endian))?;
    drain(stream, cli, 200);
    Ok(())
}

fn stop_fps(stream: &mut Box<dyn Stream>, cli: &Cli) -> Result<()> {
    write_frame(stream, &token::request_u32(token::CMD_FPS, 0, cli.endian))?;
    drain(stream, cli, 200);
    Ok(())
}

fn write_frame(stream: &mut Box<dyn Stream>, bytes: &[u8]) -> Result<()> {
    stream.write_all(bytes).context("write")?;
    stream.flush().context("flush")?;
    Ok(())
}

fn send_pair(
    stream: &mut Box<dyn Stream>,
    cli: &Cli,
    action: u32,
    group: u32,
    counter: u32,
) -> Result<()> {
    write_frame(
        stream,
        &token::request_group_counter(action, group, counter, cli.endian),
    )
}

fn run_counters(stream: &mut Box<dyn Stream>, cli: &Cli, cmd: &CounterCmd) -> Result<()> {
    match cmd {
        CounterCmd::List { group } => {
            print_counters(*group);
        }
        CounterCmd::Add {
            group,
            counter,
            reset,
        } => {
            let plan = plan_counter_add(*group, *counter, *reset);
            drain(stream, cli, 250);
            for &(g, c) in &plan.removes {
                send_pair(stream, cli, token::GPU_SCOPE_COUNTER_REMOVE, g, c)?;
            }
            send_pair(stream, cli, token::GPU_SCOPE_COUNTER_ADD, *group, *counter)?;
            println!(
                "counter add group={group} counter={counter} reset={reset} removed={}",
                plan.removes.len()
            );
            drain(stream, cli, 250);
        }
        CounterCmd::Remove { group, counter } => {
            drain(stream, cli, 250);
            send_pair(
                stream,
                cli,
                token::GPU_SCOPE_COUNTER_REMOVE,
                *group,
                *counter,
            )?;
            println!("counter remove group={group} counter={counter}");
            drain(stream, cli, 250);
        }
    }
    Ok(())
}

fn run_monitor(
    stream: &mut Box<dyn Stream>,
    cli: &Cli,
    seconds: u64,
    group: Option<u32>,
    counter: Option<u32>,
    reset: bool,
    csv: Option<&PathBuf>,
) -> Result<()> {
    let requested = match (group, counter) {
        (Some(g), Some(c)) => Some((g, c)),
        (None, None) => None,
        _ => anyhow::bail!("--group and --counter must be used together"),
    };
    let plan = plan_monitor(group, counter, reset)?;

    drain(stream, cli, 250);
    stop_scope(stream, cli)?;

    for &(g, c) in &plan.removes {
        send_pair(stream, cli, token::GPU_SCOPE_COUNTER_REMOVE, g, c)?;
    }
    if let Some((g, c)) = plan.add {
        send_pair(stream, cli, token::GPU_SCOPE_COUNTER_ADD, g, c)?;
        let line = format!(
            "monitor pair group={g} counter={c} reset={reset} removed={}",
            plan.removes.len()
        );
        if cli.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    } else {
        let line = "monitor using stock pairs; pass --group and --counter to select";
        if cli.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    std::thread::sleep(ms(50));
    write_frame(stream, &token::request_scope(2, 0, 0, cli.endian))?;
    std::thread::sleep(ms(100));

    let head_result = read_for(
        stream.as_mut(),
        Duration::from_secs(seconds),
        Duration::from_millis(200),
        cli.max_bytes,
    );
    let stop_result = write_frame(stream, &token::request(token::CMD_SCOPE_STOP, cli.endian));
    let (head, cut_head) = head_result.context("monitor read")?;
    stop_result?;
    let (tail, cut_tail) = read_for(
        stream.as_mut(),
        Duration::from_secs(2),
        Duration::from_millis(300),
        cli.max_bytes,
    )
    .context("monitor tail read")?;

    let mut all = head;
    all.extend_from_slice(&tail);
    if let Some(path) = &cli.save {
        save(path, &all)?;
    }

    let (recs, used) = record::parse_lenient(&all, cli.endian);
    if !cli.json {
        println!(
            "monitor bytes={} records={} truncated={}",
            all.len(),
            recs.len(),
            cut_head || cut_tail
        );
        if used < all.len() {
            println!("monitor tail={} bytes not framed", all.len() - used);
        }
    }
    let stats = print_monitors(
        &recs,
        cli.endian,
        requested,
        csv,
        cli.json,
        MonitorContext {
            bytes: all.len(),
            records: recs.len(),
            truncated: cut_head || cut_tail,
        },
    )?;
    ensure_nonempty("monitor", stats.reports, cli.allow_empty)
}

struct MonitorStats {
    reports: usize,
}

#[derive(Clone, Copy)]
struct MonitorContext {
    bytes: usize,
    records: usize,
    truncated: bool,
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn probe_json(data: &[u8], truncated: bool, endian: Endian) -> Value {
    let (recs, used) = record::parse_lenient(data, endian);
    let records: Vec<Value> = recs
        .iter()
        .map(|rec| {
            json!({
                "index": rec.index,
                "kind": format!("0x{:08X}", rec.kind),
                "kind_name": record::kind_name(rec.kind),
                "arg0": format!("0x{:08X}", rec.arg0),
                "payload_len": rec.payload.len(),
                "payload_hex": hex_string(rec.payload),
            })
        })
        .collect();
    json!({
        "bytes": data.len(),
        "truncated": truncated,
        "tail_bytes": data.len().saturating_sub(used),
        "records": records,
    })
}

fn scope_json(
    bytes: usize,
    truncated: bool,
    records: usize,
    timing: usize,
    submit_info: usize,
    tail_bytes: usize,
    timings: &[capture::SubmitTiming],
) -> Value {
    let timing_values: Vec<Value> = timings
        .iter()
        .map(|t| {
            json!({
                "pid": t.pid,
                "frame_index": t.frame_index,
                "start_us": t.start_us,
                "end_us": t.end_us,
                "dur_us": t.end_us.saturating_sub(t.start_us),
                "gpu_ts": t.gpu_ts,
                "frame_clock": t.frame_clock,
                "cpu_clock": t.cpu_clock,
                "gpu_ts2": t.gpu_ts2,
                "gpu_ts3": t.gpu_ts3,
            })
        })
        .collect();
    json!({
        "bytes": bytes,
        "truncated": truncated,
        "records": records,
        "timing": timing,
        "submit_info": submit_info,
        "tail_bytes": tail_bytes,
        "timings": timing_values,
    })
}

fn monitor_csv_header() -> &'static str {
    "index,frame,submit,gpu,value_group,value_counter,value,delta,clock,t0,t1,t2,raw_size,pairs,sample_count,samples"
}

fn csv_escape(text: &str) -> String {
    if text.contains(',') || text.contains('"') || text.contains('\n') || text.contains('\r') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

fn monitor_csv_row(index: u32, report: &monitor::MonitorReport<'_>, delta: Option<u64>) -> String {
    let pair = report.value_pair();
    let value_group = pair.map(|p| p.group.to_string()).unwrap_or_default();
    let value_counter = pair.map(|p| p.counter.to_string()).unwrap_or_default();
    let pairs_text = report
        .pairs
        .iter()
        .map(|p| format!("{}:{}", p.group, p.counter))
        .collect::<Vec<_>>()
        .join(",");
    let samples_text = report
        .samples
        .iter()
        .map(|s| format!("0x{:08X}={}", s.id, s.value))
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "{index},{},{},{},{},{},{},{},{},{},{},{},{},{},{},\"{}\"",
        report.frame,
        report.submit,
        report.gpu,
        value_group,
        value_counter,
        report.value,
        delta.map(|d| d.to_string()).unwrap_or_default(),
        report.clock,
        report.t0,
        report.t1,
        report.t2,
        report.raw_size,
        csv_escape(&pairs_text),
        report.samples.len(),
        csv_escape(&samples_text)
    )
}

fn monitor_json_item(index: u32, report: &monitor::MonitorReport<'_>, delta: Option<u64>) -> Value {
    let pairs: Vec<Value> = report
        .pairs
        .iter()
        .map(|p| json!({ "group": p.group, "counter": p.counter }))
        .collect();
    let value_pair = report
        .value_pair()
        .map(|p| json!({ "group": p.group, "counter": p.counter }));
    let samples: Vec<Value> = report
        .samples
        .iter()
        .map(|s| {
            json!({
                "id": format!("0x{:08X}", s.id),
                "timestamp": s.timestamp,
                "value": s.value,
                "aux": s.aux,
            })
        })
        .collect();
    json!({
        "index": index,
        "frame": report.frame,
        "submit": report.submit,
        "gpu": report.gpu,
        "pairs": pairs,
        "value_pair": value_pair,
        "value": report.value,
        "delta": delta,
        "clock": report.clock,
        "t0": report.t0,
        "t1": report.t1,
        "t2": report.t2,
        "raw_size": report.raw_size,
        "samples": samples,
    })
}

fn print_monitors(
    recs: &[Record<'_>],
    endian: Endian,
    requested: Option<(u32, u32)>,
    csv: Option<&PathBuf>,
    json: bool,
    context: MonitorContext,
) -> Result<MonitorStats> {
    let mut file = match csv {
        Some(path) => {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .with_context(|| format!("open {path:?}"))?;
            writeln!(file, "{}", monitor_csv_header())?;
            Some(file)
        }
        None => None,
    };

    let mut prev: HashMap<(u32, u32), u64> = HashMap::new();
    let mut reports = 0usize;
    let mut done = 0usize;
    let mut warned = false;
    let mut items = Vec::new();

    for rec in recs {
        if rec.kind == record::KIND_MONITORS_DONE {
            done += 1;
            if !json {
                println!("monitor [{}] monitors_done", rec.index);
            }
            continue;
        }
        if rec.kind != record::KIND_MONITOR {
            continue;
        }
        let report = match monitor::parse(rec.payload, endian) {
            Some(report) => report,
            None => {
                if !json {
                    println!(
                        "monitor [{}] undecoded payload={}",
                        rec.index,
                        rec.payload.len()
                    );
                }
                continue;
            }
        };
        reports += 1;

        let pair = report.value_pair();
        let key = pair.map(|p| (p.group, p.counter)).unwrap_or((0, 0));
        let delta = prev
            .insert(key, report.value)
            .map(|old| report.value.saturating_sub(old));

        if json {
            items.push(monitor_json_item(rec.index, &report, delta));
        } else {
            let line = monitor_line(rec.index, &report, delta);
            println!("{line}");

            if let Some((g, c)) = requested {
                let first = pair.map(|p| (p.group, p.counter));
                if !warned && report.pair_index(g, c).is_some() && first != Some((g, c)) {
                    println!(
                        "  note: cumulative value belongs to pair {:?}; requested pair {g}:{c} is not first",
                        first
                    );
                    warned = true;
                }
            }

            if !report.samples.is_empty() {
                let samples: Vec<String> = report
                    .samples
                    .iter()
                    .map(|s| format!("0x{:08X}={}", s.id, s.value))
                    .collect();
                println!("  samples {}", samples.join(" "));
            }
        }

        if let Some(file) = file.as_mut() {
            writeln!(file, "{}", monitor_csv_row(rec.index, &report, delta))?;
        }
    }

    if json {
        print_json(&json!({
            "bytes": context.bytes,
            "records": context.records,
            "truncated": context.truncated,
            "reports": reports,
            "done_markers": done,
            "items": items,
        }))?;
    } else {
        println!("monitor reports={reports} done_markers={done}");
        if let Some(path) = csv {
            println!("monitor csv reports={reports} -> {path:?}");
        }
    }
    Ok(MonitorStats { reports })
}

fn monitor_line(index: u32, report: &monitor::MonitorReport<'_>, delta: Option<u64>) -> String {
    let value_pair = report
        .value_pair()
        .map(|p| format!("{}:{}", p.group, p.counter))
        .unwrap_or_else(|| "-".to_string());
    let pairs = if report.pairs.is_empty() {
        "-".to_string()
    } else {
        report
            .pairs
            .iter()
            .map(|p| format!("{}:{}", p.group, p.counter))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "monitor [{}] frame={} submit={} gpu={} pairs={} value_pair={} value={} delta={} clock=0x{:X} t0={} t1={} t2={} raw={}",
        index,
        report.frame,
        report.submit,
        report.gpu,
        pairs,
        value_pair,
        report.value,
        delta.map(|d| d.to_string()).unwrap_or_else(|| "-".to_string()),
        report.clock,
        report.t0,
        report.t1,
        report.t2,
        report.raw_size
    )
}

fn print_counters(group: Option<u32>) {
    match group {
        Some(id) => {
            let block = BLOCKS.iter().find(|b| b.0 == id);
            match block {
                Some((_, name, count)) => {
                    println!("block {id} {name} counters={count}");
                    let mut any = false;
                    for &(g, c, name) in COUNTERS {
                        if g == id {
                            println!("  {g:>3} {c:>3} {name}");
                            any = true;
                        }
                    }
                    if !any {
                        println!(
                            "  no curated names; counter indexes 0..{} may be valid",
                            count.saturating_sub(1)
                        );
                    }
                }
                None => println!("unknown GSL block {id}"),
            }
        }
        None => {
            println!("GSL block  name        counters  curated");
            for &(id, name, count) in BLOCKS {
                let curated = COUNTERS.iter().filter(|c| c.0 == id).count();
                println!("{id:>9}  {name:<10} {count:>8}  {curated:>7}");
            }
            println!();
            println!("Use --group N to list curated counters for one block.");
            println!("Then pass --group N --counter M to 'monitor' or 'counters add'.");
            println!("IDs are A8x GSL block/counter values; other Adreno families differ.");
        }
    }
}

fn adb_args(serial: Option<&str>, args: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(serial) = serial {
        out.push("-s".to_string());
        out.push(serial.to_string());
    }
    out.extend(args.iter().map(|arg| arg.to_string()));
    out
}

fn run_adb(serial: Option<&str>, args: &[&str]) -> Result<()> {
    let output = Command::new("adb")
        .args(adb_args(serial, args))
        .output()
        .context("run adb")?;
    if !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        anyhow::bail!("adb {:?} failed: {}", args, output.status);
    }
    Ok(())
}

fn perfetto_paths(pid: u32) -> (String, String) {
    (
        format!("/data/misc/perfetto-configs/qstream-{pid}.cfg"),
        format!("/data/misc/perfetto-traces/qstream-{pid}.pb"),
    )
}

fn run_perfetto(
    seconds: u64,
    out: &PathBuf,
    config: Option<&PathBuf>,
    serial: Option<&str>,
) -> Result<()> {
    let pid = std::process::id();
    let local = std::env::temp_dir().join(format!("qstream-perfetto-{pid}.cfg"));
    match config {
        Some(src) => {
            std::fs::copy(src, &local).context("copy perfetto config")?;
        }
        None => {
            let text = format!(
                "buffers: {{ size_kb: 8192 }}\n\
                 data_sources: {{ config {{ name: \"gpu.renderstages\" }} }}\n\
                 duration_ms: {}\n",
                seconds.saturating_mul(1000)
            );
            std::fs::write(&local, text).context("write perfetto config")?;
        }
    }

    let (remote_cfg, remote_out) = perfetto_paths(pid);
    let local_str = local.to_str().context("config path")?;
    let out_str = out.to_str().context("out path")?;

    let result = (|| -> Result<()> {
        run_adb(serial, &["push", local_str, remote_cfg.as_str()])?;
        run_adb(
            serial,
            &[
                "shell",
                "perfetto",
                "--txt",
                "-c",
                remote_cfg.as_str(),
                "-o",
                remote_out.as_str(),
            ],
        )?;
        run_adb(serial, &["pull", remote_out.as_str(), out_str])?;
        Ok(())
    })();
    let _ = run_adb(
        serial,
        &[
            "shell",
            "rm",
            "-f",
            remote_cfg.as_str(),
            remote_out.as_str(),
        ],
    );
    let _ = std::fs::remove_file(&local);
    result?;

    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!("perfetto trace {out:?} bytes={size}");
    if size < 1024 {
        println!(
            "perfetto note: no packets; the layer only registers gpu.renderstages in direct load mode"
        );
    }
    Ok(())
}

fn parse_ptr(text: &str) -> Result<u64> {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u64::from_str_radix(t, 16).with_context(|| format!("parse pointer {text:?}"))
}

fn send(stream: &mut Box<dyn Stream>, bytes: &[u8]) -> Result<()> {
    write_frame(stream, bytes)?;
    println!("tx {} bytes", bytes.len());
    print!("{}", hex::dump(bytes));
    Ok(())
}

fn send_cmd(stream: &mut Box<dyn Stream>, bytes: &[u8], cli: &Cli) -> Result<()> {
    if cli.json {
        write_frame(stream, bytes)
    } else {
        send(stream, bytes)
    }
}

fn save(path: &PathBuf, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {:?}", path))?;
    file.write_all(data).context("save")?;
    Ok(())
}

fn finish_reply(data: Vec<u8>, truncated: bool, cli: &Cli, mode: ReplyMode) -> Result<Reply> {
    if !data.is_empty() {
        if let Some(path) = &cli.save {
            save(path, &data)?;
        }
    }

    if data.is_empty() {
        if mode == ReplyMode::Required {
            ensure_nonempty("reply", 0, cli.allow_empty)?;
        }
        if !cli.json {
            println!("rx 0 bytes (no reply)");
        }
        return Ok(Reply { data, truncated });
    }

    if !cli.json {
        show_reply(&data, truncated, cli);
    }

    Ok(Reply { data, truncated })
}

fn read(stream: &mut Box<dyn Stream>, cli: &Cli, mode: ReplyMode) -> Result<Reply> {
    let (data, truncated) =
        read_until_idle(stream.as_mut(), ms(cli.idle_ms), cli.max_bytes).context("read")?;
    finish_reply(data, truncated, cli, mode)
}

fn read_window(
    stream: &mut dyn Stream,
    cli: &Cli,
    total: Duration,
    slice: Duration,
    what: &str,
    mode: ReplyMode,
) -> Result<Reply> {
    let (data, truncated) =
        read_for(stream, total, slice, cli.max_bytes).with_context(|| format!("{what} read"))?;
    finish_reply(data, truncated, cli, mode)
}

fn show_reply(data: &[u8], truncated: bool, cli: &Cli) {
    println!(
        "rx {} bytes{}",
        data.len(),
        if truncated { " (truncated)" } else { "" }
    );
    print!("{}", hex::dump(data));

    if data.len() >= 4 {
        let tok = cli.endian.from_bytes([data[0], data[1], data[2], data[3]]);
        println!("token 0x{tok:08X} ({})", token::name(tok));
    }

    if let Ok(recs) = record::parse(data, cli.endian) {
        show_records(&recs, cli.endian);
    }
}

fn show_records(recs: &[Record<'_>], endian: Endian) {
    println!("records {}", recs.len());
    for rec in recs {
        println!(
            "  [{}] kind=0x{:08X} ({}) payload={} arg0=0x{:08X}",
            rec.index,
            rec.kind,
            record::kind_name(rec.kind),
            rec.payload.len(),
            rec.arg0
        );
        match rec.kind {
            record::KIND_PROCESS => {
                println!(
                    "      pid={} text={:?}",
                    word(rec.payload, 4, endian),
                    record::cstr(rec.payload, 8)
                );
            }
            record::KIND_DEVICE => {
                println!("      text={:?}", record::cstr(rec.payload, 20));
            }
            record::KIND_COUNTERS => {
                println!("      words={:?}", words(rec.payload, 4, endian));
            }
            record::KIND_MONITOR => match monitor::parse(rec.payload, endian) {
                Some(report) => {
                    println!("      {}", monitor_line(rec.index, &report, None));
                    if !report.samples.is_empty() {
                        let samples: Vec<String> = report
                            .samples
                            .iter()
                            .map(|s| format!("0x{:08X}={}", s.id, s.value))
                            .collect();
                        println!("      samples {}", samples.join(" "));
                    }
                }
                None => println!("      monitor payload={} undecoded", rec.payload.len()),
            },
            record::KIND_MONITORS_DONE => println!("      monitors done"),
            record::KIND_VERSION => {
                println!("      words={:?}", words(rec.payload, 14, endian));
            }
            record::KIND_FRAME_START | record::KIND_FRAME_END => {
                println!(
                    "      marker={} device=0x{:08X}{:08X}",
                    word(rec.payload, 4, endian),
                    word(rec.payload, 12, endian),
                    word(rec.payload, 8, endian)
                );
            }
            record::KIND_FRAME_BATCH => match capture::parse_tokens(rec.payload, endian) {
                Ok(tokens) => {
                    let timing = tokens
                        .iter()
                        .filter(|t| capture::submit_timing(t, endian).is_some())
                        .count();
                    let submits = tokens
                        .iter()
                        .filter(|t| capture::submit_info(t, endian).is_some())
                        .count();
                    println!(
                        "      state_tokens={} timing={timing} submit_info={submits}",
                        tokens.len()
                    );
                }
                Err(err) => println!("      batch parse failed: {err}"),
            },
            _ => {}
        }
    }
}

fn word(buf: &[u8], off: usize, endian: Endian) -> u32 {
    if buf.len() < off + 4 {
        return 0;
    }
    endian.from_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn words(buf: &[u8], count: usize, endian: Endian) -> Vec<u32> {
    let count = count.min(buf.len() / 4);
    (0..count).map(|i| word(buf, i * 4, endian)).collect()
}

/// Valid GSL blocks on Adreno 8xx (A8x): id, name, counter count.
const BLOCKS: &[(u32, &str, u32)] = &[
    (0, "CP", 91),
    (1, "RBBM", 17),
    (2, "PC", 87),
    (3, "VFD", 28),
    (4, "HLSQ", 137),
    (5, "VPC", 71),
    (6, "TSE", 119),
    (7, "RAS", 39),
    (8, "UCHE", 203),
    (9, "TP", 143),
    (10, "SP", 205),
    (11, "RB", 56),
    (13, "GBIF", 207),
    (14, "GBIF_PWR", 4),
    (23, "VSC", 8),
    (24, "CCU", 137),
    (25, "LRZ", 34),
    (26, "CMP", 63),
    (27, "ALWAYSON", 1),
    (39, "GMU_XOCLK", 96),
    (40, "GMU_GMUCLK", 39),
    (41, "GMU_PERF", 24),
    (43, "UFC", 49),
    (44, "BV_CP", 91),
    (45, "BV_PC", 87),
    (46, "BV_VFD", 28),
    (47, "BV_VPC", 71),
    (48, "BV_TP", 143),
    (49, "BV_SP", 205),
    (50, "BV_UFC", 49),
    (51, "BV_TSE", 119),
    (52, "BV_RAS", 39),
    (53, "BV_LRZ", 34),
    (54, "BV_HLSQ", 137),
];

/// Common A8x counters: GSL block, counter index, driver name.
const COUNTERS: &[(u32, u32, &str)] = &[
    (0, 0, "PERF_CP_NEVER_COUNT"),
    (0, 1, "PERF_CP_ALWAYS_COUNT"),
    (0, 2, "PERF_CP_BUSY_GFX_CORE_IDLE"),
    (0, 3, "PERF_CP_BUSY_CYCLES"),
    (0, 4, "PERF_CP_NUM_PREEMPTIONS"),
    (1, 1, "PERF_RBBM_US_ALWAYS_COUNT"),
    (1, 2, "PERF_RBBM_US_ALWAYS_ON"),
    (1, 3, "PERF_RBBM_US_STATUS_MASKED"),
    (2, 1, "PERF_PC_US_BUSY_CYCLES"),
    (2, 2, "PERF_PC_US_WORKING_CYCLES"),
    (3, 1, "PERF_VFD_BUSY_CYCLES"),
    (4, 1, "PERF_HLSQ_BUSY_CYCLES"),
    (4, 2, "PERF_HLSQ_STALL_CYCLES_SP_STATE"),
    (4, 3, "PERF_HLSQ_STALL_CYCLES_SP_FS_STAGE"),
    (5, 1, "PERF_VPC_FE_BUSY_CYCLES"),
    (5, 2, "PERF_VPC_FE_WORKING_CYCLES"),
    (6, 1, "PERF_TSE_BE_BUSY_CYCLES"),
    (6, 2, "PERF_TSE_BE_CLIPPING_CYCLES"),
    (7, 1, "PERF_RAS_BUSY_CYCLES"),
    (7, 2, "PERF_RAS_SUPERTILE_ACTIVE_CYCLES"),
    (8, 1, "PERF_UCHE_BUSY_CYCLES"),
    (8, 2, "PERF_UCHE_STALL_CYCLES_ARBITER"),
    (9, 1, "PERF_TP_BUSY_CYCLES"),
    (9, 2, "PERF_TP_STALL_CYCLES_UCHE"),
    (9, 3, "PERF_TP_LATENCY_CYCLES"),
    (9, 4, "PERF_TP_LATENCY_TRANS"),
    (9, 11, "PERF_TP_OUTPUT_PIXELS"),
    (10, 1, "PERF_SP_BUSY_CYCLES"),
    (10, 2, "PERF_SP_ALU_WORKING_CYCLES"),
    (10, 3, "PERF_SP_STALL_CYCLES_VPC_BE"),
    (10, 4, "PERF_SP_STALL_CYCLES_TP"),
    (10, 5, "PERF_SP_STALL_CYCLES_UCHE"),
    (10, 6, "PERF_SP_STALL_CYCLES_RB"),
    (10, 7, "PERF_SP_NON_EXECUTION_CYCLES"),
    (10, 8, "PERF_SP_WAVE_CONTEXTS"),
    (11, 1, "PERF_RB_BUSY_CYCLES"),
    (11, 2, "PERF_RB_STALL_CYCLES_HLSQ"),
    (11, 3, "PERF_RB_STALL_CYCLES_FIFO0_FULL"),
    (11, 4, "PERF_RB_STALL_CYCLES_FIFO1_FULL"),
    (11, 5, "PERF_RB_STALL_CYCLES_FIFO2_FULL"),
    (11, 6, "PERF_RB_STARVE_CYCLES_SP"),
    (11, 7, "PERF_RB_STARVE_CYCLES_LRZ_TILE"),
    (11, 8, "PERF_RB_STARVE_CYCLES_CCU"),
    (13, 10, "PERF_GBIF_AXI0_READ_REQUESTS_TOTAL"),
    (13, 11, "PERF_GBIF_AXI1_READ_REQUESTS_TOTAL"),
    (14, 0, "M0_STALL"),
    (14, 1, "M1_STALL"),
    (14, 2, "M0_NO_CLIENT_REQ"),
    (14, 3, "M1_NO_CLIENT_REQ"),
    (23, 1, "PERF_VSC_BUSY_CYCLES"),
    (23, 2, "PERF_VSC_WORKING_CYCLES"),
    (24, 1, "PERF_CCU_BUSY_CYCLES"),
    (24, 4, "PERF_CCU_DEPTH_BLOCKS"),
    (24, 5, "PERF_CCU_COLOR_BLOCKS"),
    (25, 1, "PERF_LRZ_BUSY_CYCLES"),
    (25, 2, "PERF_LRZ_STARVE_CYCLES_RAS"),
    (25, 3, "PERF_LRZ_STALL_CYCLES_RB"),
    (26, 0, "PERF_CMPDECMP_NEVER_COUNT"),
    (26, 1, "PERF_CMPDECMP_STALL_CYCLES_ARB"),
    (27, 0, "PERF_ALWAYSON"),
    (39, 1, "PERF_GMU_PWC_GPU_TOP_POWER_ON_CYCLES"),
    (39, 2, "PERF_GMU_PWC_GPU_TOP_POWER_OFF_SWITCHING_CYCLES"),
    (40, 1, "PERF_GMU_PWC_FW_INCREMENT_0"),
    (41, 1, "PERF_GMU_CM3_BUSY_CYCLES"),
    (43, 1, "PERF_UFC_BUSY_CYCLES"),
    (44, 3, "PERF_CP_BUSY_CYCLES"),
    (45, 1, "PERF_PC_US_BUSY_CYCLES"),
    (46, 1, "PERF_VFD_BUSY_CYCLES"),
    (47, 1, "PERF_VPC_FE_BUSY_CYCLES"),
    (48, 1, "PERF_TP_BUSY_CYCLES"),
    (49, 1, "PERF_SP_BUSY_CYCLES"),
    (50, 1, "PERF_UFC_BUSY_CYCLES"),
    (51, 1, "PERF_TSE_BE_BUSY_CYCLES"),
    (52, 1, "PERF_RAS_BUSY_CYCLES"),
    (53, 1, "PERF_LRZ_BUSY_CYCLES"),
    (54, 1, "PERF_HLSQ_BUSY_CYCLES"),
];

fn print_tokens() {
    let rows = [
        (token::CMD_REGISTER, "register"),
        (token::CMD_GPU_SCOPE_GROUPS, "gpu scope counter groups"),
        (token::CMD_SCOPE_START, "gpu scope start + version"),
        (token::CMD_SCOPE_STOP, "gpu scope stop"),
        (token::CMD_CAPTURE_FRAME, "capture next frame"),
        (0x0102_0004, "frame start marker"),
        (0x0102_0005, "frame end marker"),
        (token::CMD_CAPTURE_START, "capture start"),
        (token::CMD_CAPTURE_STOP, "capture stop"),
        (token::CMD_FPS, "fps stream"),
        (token::CMD_FRAME_RATE, "frame rate"),
        (token::CMD_FRAME_DELIMITERS, "frame delimiters"),
        (0x0B02_0004, "capture started ack"),
        (0x0B02_0005, "capture stopped ack"),
        (0x0B02_000E, "capture batch (submit tokens)"),
        (0x0B02_0007, "gpu scope monitor report"),
        (0x0102_001C, "monitors done marker"),
        (0x0B05_0013, "queue submit timing"),
        (0x0B03_006B, "VkSubmitInfo state"),
        (token::REPLY_REGISTER_OK, "record magic"),
        (token::REPLY_VERSION, "version reply"),
    ];
    println!("token         kind      meaning");
    for (tok, desc) in rows {
        let kind = if tok == token::REPLY_REGISTER_OK {
            "record"
        } else if token::name(tok).starts_with("reply") {
            "reply"
        } else if matches!(
            tok,
            0x0102_0004
                | 0x0102_0005
                | 0x0B02_0004
                | 0x0B02_0005
                | 0x0B02_000E
                | 0x0B02_0007
                | 0x0102_001C
                | 0x0B05_0013
                | 0x0B03_006B
        ) {
            "state"
        } else {
            "cmd"
        };
        println!("0x{tok:08X}  {kind:<8}  {desc}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qstream_core::monitor::{CounterPair, MonitorReport, MonitorSample};

    fn sample_report(raw: &[u8]) -> MonitorReport<'_> {
        MonitorReport {
            gpu: 39,
            mode: 2,
            field: 3,
            arg: 0x491F_0910,
            object: 0xB400_0079_491F_09B0,
            submit: 387,
            flags: 0,
            frame: 139,
            arg2: 0xB400_0079_491F_0910,
            raw_size: raw.len(),
            pairs: vec![CounterPair {
                group: 9,
                counter: 1,
            }],
            clock: 0x6AEC_72A9,
            value: 766_081_486,
            t0: 1,
            t1: 2,
            t2: 3,
            samples: vec![
                MonitorSample {
                    id: 0x0008_0001,
                    timestamp: 10,
                    value: 9_562,
                    aux: 1,
                },
                MonitorSample {
                    id: 0x0008_0002,
                    timestamp: 11,
                    value: 48_577,
                    aux: 2,
                },
            ],
            raw,
        }
    }

    #[test]
    fn counter_add_keeps_existing_pairs() {
        let plan = plan_counter_add(9, 1, false);
        assert_eq!(plan.removes, vec![(9, 1)]);
        assert_eq!(plan.add, Some((9, 1)));
        assert!(!plan.reset);
    }

    #[test]
    fn counter_add_reset_removes_curated() {
        let plan = plan_counter_add(9, 1, true);
        assert_eq!(plan.removes.len(), COUNTERS.len());
        assert!(plan.removes.contains(&(9, 1)));
    }

    #[test]
    fn monitor_default_removes_stock_pairs() {
        let plan = plan_monitor(Some(9), Some(1), false).unwrap();
        assert_eq!(plan.removes, vec![(0, 0), (27, 0), (9, 1)]);
    }

    #[test]
    fn monitor_reset_removes_curated() {
        let plan = plan_monitor(Some(9), Some(1), true).unwrap();
        assert_eq!(plan.removes.len(), COUNTERS.len());
        assert!(plan.removes.contains(&(9, 1)));
    }

    #[test]
    fn monitor_without_pair_keeps_pairs() {
        let plan = plan_monitor(None, None, true).unwrap();
        assert!(plan.removes.is_empty());
        assert_eq!(plan.add, None);
    }

    #[test]
    fn monitor_rejects_half_pair() {
        assert!(plan_monitor(Some(9), None, false).is_err());
        assert!(plan_monitor(None, Some(1), false).is_err());
    }

    #[test]
    fn allow_empty_controls_exit() {
        assert!(ensure_nonempty("reply", 0, false).is_err());
        assert!(ensure_nonempty("reply", 0, true).is_ok());
        assert!(ensure_nonempty("reply", 1, false).is_ok());
    }

    #[test]
    fn frame_capture_does_not_stop() {
        assert!(!capture_needs_stop(true));
        assert!(capture_needs_stop(false));
    }

    #[test]
    fn probe_json_exposes_records() {
        let data =
            hex::parse(include_str!("../../qstream-core/tests/fixtures/register.hex").trim())
                .unwrap();
        let value = probe_json(&data, false, Endian::Little);
        assert_eq!(value["tail_bytes"], 0);
        assert_eq!(value["records"].as_array().unwrap().len(), 3);
        assert_eq!(value["records"][0]["kind"], "0x0B010001");
    }

    #[test]
    fn monitor_json_exposes_samples() {
        let item = monitor_json_item(7, &sample_report(&[]), Some(123));
        assert_eq!(item["value_pair"]["group"], 9);
        assert_eq!(item["delta"], 123);
        assert_eq!(item["samples"].as_array().unwrap().len(), 2);
        assert_eq!(item["samples"][0]["value"], 9_562);
    }

    #[test]
    fn monitor_csv_includes_samples() {
        let row = monitor_csv_row(7, &sample_report(&[]), Some(123));
        assert!(row.ends_with(",2,\"0x00080001=9562;0x00080002=48577\""));
        assert!(row.contains(",9,1,766081486,123,"));
    }

    #[test]
    fn scope_json_exposes_timings() {
        let timing = capture::SubmitTiming {
            pid: 301,
            frame_index: 5,
            field_8: 0,
            start_us: 10,
            end_us: 15,
            gpu_ts: 20,
            frame_clock: 30,
            cpu_clock: 40,
            gpu_ts2: 50,
            gpu_ts3: 60,
        };
        let value = scope_json(100, false, 2, 1, 0, 0, &[timing]);
        assert_eq!(value["timings"][0]["dur_us"], 5);
        assert_eq!(value["timings"][0]["frame_index"], 5);
    }

    #[test]
    fn adb_args_prepends_serial() {
        assert_eq!(
            adb_args(Some("ABC"), &["shell", "true"]),
            vec!["-s", "ABC", "shell", "true"]
        );
        assert_eq!(adb_args(None, &["devices"]), vec!["devices"]);
    }

    #[test]
    fn perfetto_paths_are_unique() {
        let (cfg, trace) = perfetto_paths(42);
        assert!(cfg.ends_with("qstream-42.cfg"));
        assert!(trace.ends_with("qstream-42.pb"));
    }

    #[test]
    fn csv_escape_quotes_special_fields() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_escape("plain"), "plain");
    }
}
