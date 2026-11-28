#![expect(
    clippy::print_stdout,
    reason = "Benchmark CLI prints comparison tables"
)]
#![expect(
    clippy::print_stderr,
    reason = "Benchmark CLI prints progress diagnostics"
)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_CASES_PATH: &str = "bench/cases.json";
const PROJECT_ID: &str = "test";

#[derive(Debug, Deserialize)]
struct BenchCase {
    name: String,
    category: String,
    #[serde(default)]
    heavy: bool,
    setup: Vec<String>,
    #[serde(default)]
    pre_iter_drop: Option<String>,
    query: String,
    #[serde(default = "default_warmup")]
    warmup: usize,
    #[serde(default = "default_iterations")]
    iterations: usize,
}

const fn default_warmup() -> usize {
    2
}

const fn default_iterations() -> usize {
    10
}

#[derive(Debug, Clone, Serialize)]
struct CaseMetric {
    status: String,
    p50_ms: Option<f64>,
    p95_ms: Option<f64>,
    min_ms: Option<f64>,
    rows: Option<usize>,
    error: Option<String>,
}

impl CaseMetric {
    fn err(msg: String) -> Self {
        Self {
            status: "error".to_owned(),
            p50_ms: None,
            p95_ms: None,
            min_ms: None,
            rows: None,
            error: Some(msg),
        }
    }
}

struct CliOpts {
    targets: Vec<String>,
    cases_path: PathBuf,
    include_heavy: bool,
    filter: Option<String>,
    out_json: Option<PathBuf>,
    urls: BTreeMap<String, String>,
    timeout_secs: u64,
}

struct ManagedTarget {
    name: String,
    base_url: String,
    needs_drop_before_replace: bool,
    child: Option<Child>,
    docker_name: Option<String>,
}

impl Drop for ManagedTarget {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(ref cname) = self.docker_name {
            let _ = Command::new("docker")
                .args(["rm", "-f", cname])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn parse_args() -> Result<CliOpts, String> {
    let mut targets = Vec::new();
    let mut cases_path = PathBuf::from(DEFAULT_CASES_PATH);
    let mut include_heavy = false;
    let mut filter = None;
    let mut out_json = None;
    let mut urls = BTreeMap::new();
    let mut timeout_secs = 60_u64;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" | "-t" => {
                let v = args.next().ok_or("--target requires a value")?;
                for part in v.split(',') {
                    if !part.is_empty() {
                        targets.push(part.to_owned());
                    }
                }
            }
            "--cases" => {
                cases_path = PathBuf::from(args.next().ok_or("--cases requires a path")?);
            }
            "--include-heavy" => include_heavy = true,
            "--case" | "--filter" => {
                filter = Some(args.next().ok_or("--filter requires a substring")?);
            }
            "--out" => {
                out_json = Some(PathBuf::from(args.next().ok_or("--out requires a path")?));
            }
            "--bq-lite-url" => {
                urls.insert(
                    "bq-lite".to_owned(),
                    args.next().ok_or("--bq-lite-url requires a URL")?,
                );
            }
            "--vantaboard-url" => {
                urls.insert(
                    "vantaboard".to_owned(),
                    args.next().ok_or("--vantaboard-url requires a URL")?,
                );
            }
            "--goccy-url" => {
                urls.insert(
                    "goccy".to_owned(),
                    args.next().ok_or("--goccy-url requires a URL")?,
                );
            }
            "--timeout" => {
                let v = args.next().ok_or("--timeout requires seconds")?;
                timeout_secs = v.parse().map_err(|_| "invalid --timeout value")?;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: bench_emulators [--target bq-lite,vantaboard,goccy] [--include-heavy] [--filter SUBSTR] [--out results.json]"
                );
                return Err(String::new());
            }
            other => return Err(format!("Unknown argument: {other}")),
        }
    }
    if targets.is_empty() {
        targets.push("bq-lite".to_owned());
    }
    Ok(CliOpts {
        targets,
        cases_path,
        include_heavy,
        filter,
        out_json,
        urls,
        timeout_secs,
    })
}

fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind failed: {e}"))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("local_addr failed: {e}"))
}

async fn wait_ready(client: &reqwest::Client, base_url: &str, name: &str) -> Result<(), String> {
    let probe_url = format!("{base_url}/bigquery/v2/projects/{PROJECT_ID}/queries");
    let body = json!({ "query": "SELECT 1", "useLegacySql": false });
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        if let Ok(resp) = client
            .post(&probe_url)
            .json(&body)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "Target '{name}' at {base_url} did not become ready within 45s"
    ))
}

async fn start_target(
    name: &str,
    explicit_url: Option<&String>,
    client: &reqwest::Client,
) -> Result<ManagedTarget, String> {
    if let Some(url) = explicit_url {
        let base_url = url.trim_end_matches('/').to_owned();
        wait_ready(client, &base_url, name).await?;
        return Ok(ManagedTarget {
            name: name.to_owned(),
            base_url,
            needs_drop_before_replace: name == "goccy",
            child: None,
            docker_name: None,
        });
    }
    let http_port = free_port()?;
    match name {
        "bq-lite" => {
            let grpc_port = free_port()?;
            let bin = std::env::var("BQ_LITE_BIN")
                .unwrap_or_else(|_| "target/release/bq-lite-server".to_owned());
            let child = Command::new(&bin)
                .args([
                    "--memory",
                    "--quiet",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &http_port.to_string(),
                    "--grpc-port",
                    &grpc_port.to_string(),
                    "--project",
                    PROJECT_ID,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to spawn {bin}: {e}"))?;
            let base_url = format!("http://127.0.0.1:{http_port}");
            let target = ManagedTarget {
                name: name.to_owned(),
                base_url,
                needs_drop_before_replace: false,
                child: Some(child),
                docker_name: None,
            };
            wait_ready(client, &target.base_url, name).await?;
            Ok(target)
        }
        "vantaboard" => {
            start_docker_target(
                client,
                name,
                http_port,
                "ghcr.io/vantaboard/bigquery-emulator:latest",
                &["--platform=linux/amd64", "-p"],
                "9050",
                &[],
                false,
            )
            .await
        }
        "goccy" => {
            start_docker_target(
                client,
                name,
                http_port,
                "ghcr.io/goccy/bigquery-emulator:0.8.1",
                &["-p"],
                "9050",
                &["--project", PROJECT_ID],
                true,
            )
            .await
        }
        other => Err(format!(
            "Unsupported target '{other}' (expected bq-lite, vantaboard, or goccy)"
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Docker container launch configuration bundles image, ports, args, and target flags"
)]
async fn start_docker_target(
    client: &reqwest::Client,
    name: &str,
    http_port: u16,
    image: &str,
    prefix_args: &[&str],
    container_port: &str,
    extra_cmd: &[&str],
    needs_drop: bool,
) -> Result<ManagedTarget, String> {
    let cname = format!("bq-bench-{name}-{http_port}");
    eprintln!("==> Starting Docker target {name} ({image}) on 127.0.0.1:{http_port}");
    let mut cmd = Command::new("docker");
    cmd.args(["run", "-d", "--rm", "--name", &cname]);
    for a in prefix_args {
        cmd.arg(a);
    }
    cmd.arg(format!("127.0.0.1:{http_port}:{container_port}"));
    cmd.arg(image);
    for a in extra_cmd {
        cmd.arg(a);
    }
    let status = cmd
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("docker run failed for {name}: {e}"))?;
    if !status.success() {
        return Err(format!("docker run exited with {status} for {name}"));
    }
    let target = ManagedTarget {
        name: name.to_owned(),
        base_url: format!("http://127.0.0.1:{http_port}"),
        needs_drop_before_replace: needs_drop,
        child: None,
        docker_name: Some(cname),
    };
    wait_ready(client, &target.base_url, name).await?;
    Ok(target)
}

async fn create_dataset(
    client: &reqwest::Client,
    base_url: &str,
    dataset: &str,
) -> Result<(), String> {
    let url = format!("{base_url}/bigquery/v2/projects/{PROJECT_ID}/datasets");
    let body = json!({ "datasetReference": { "projectId": PROJECT_ID, "datasetId": dataset } });
    let resp = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("create dataset failed: {e}"))?;
    if resp.status().is_success() || resp.status().as_u16() == 409 {
        Ok(())
    } else {
        let text = resp.text().await.unwrap_or_default();
        Err(format!("create dataset failed: {text}"))
    }
}

async fn run_query(
    client: &reqwest::Client,
    base_url: &str,
    sql: &str,
    timeout_secs: u64,
) -> Result<usize, String> {
    let url = format!("{base_url}/bigquery/v2/projects/{PROJECT_ID}/queries");
    let body = json!({
        "query": sql,
        "useLegacySql": false,
        "useQueryCache": false,
        "maxResults": 10000
    });
    let resp = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;
    let status = resp.status();
    let val: Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid JSON ({status}): {e}"))?;
    if !status.is_success() || val.get("error").is_some() {
        let msg = val
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("query error");
        return Err(format!("HTTP {status}: {msg}"));
    }
    let rows = val
        .get("rows")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Ok(rows)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Benchmark sample counts are small positive integers"
    )]
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted.get(idx).copied().unwrap_or(0.0)
}

async fn run_case_on_target(
    client: &reqwest::Client,
    target: &ManagedTarget,
    case: &BenchCase,
    timeout_secs: u64,
) -> CaseMetric {
    let ds = format!("bench_{}", case.name);
    if let Err(e) = create_dataset(client, &target.base_url, &ds).await {
        return CaseMetric::err(e);
    }
    for sql in &case.setup {
        let rendered = sql.replace("{{ds}}", &ds);
        if let Err(e) = run_query(client, &target.base_url, &rendered, timeout_secs.max(120)).await
        {
            return CaseMetric::err(format!("setup: {e}"));
        }
    }
    let query = case.query.replace("{{ds}}", &ds);
    let drop_sql = case
        .pre_iter_drop
        .as_ref()
        .map(|s| s.replace("{{ds}}", &ds));
    for _ in 0..case.warmup {
        if target.needs_drop_before_replace {
            if let Some(ref d) = drop_sql {
                let _ = run_query(client, &target.base_url, d, timeout_secs).await;
            }
        }
        if let Err(e) = run_query(client, &target.base_url, &query, timeout_secs).await {
            return CaseMetric::err(e);
        }
    }
    let mut times = Vec::with_capacity(case.iterations);
    let mut row_count = 0_usize;
    for _ in 0..case.iterations {
        if target.needs_drop_before_replace {
            if let Some(ref d) = drop_sql {
                let _ = run_query(client, &target.base_url, d, timeout_secs).await;
            }
        }
        let t0 = Instant::now();
        match run_query(client, &target.base_url, &query, timeout_secs).await {
            Ok(n) => {
                times.push(t0.elapsed().as_secs_f64() * 1000.0);
                row_count = n;
            }
            Err(e) => return CaseMetric::err(e),
        }
    }
    times.sort_by(f64::total_cmp);
    CaseMetric {
        status: "ok".to_owned(),
        p50_ms: Some(percentile(&times, 0.50)),
        p95_ms: Some(percentile(&times, 0.95)),
        min_ms: times.first().copied(),
        rows: Some(row_count),
        error: None,
    }
}

fn print_markdown_table(
    cases: &[&BenchCase],
    target_names: &[String],
    results: &BTreeMap<String, BTreeMap<String, CaseMetric>>,
) {
    print!("\n| Case | Category | Rows |");
    for name in target_names {
        print!(" {name} p50 |");
    }
    if target_names.len() >= 2 && target_names.first().map(String::as_str) == Some("bq-lite") {
        for other in target_names.iter().skip(1) {
            print!(" vs {other} |");
        }
    }
    println!();
    print!("|---|---|---:|");
    for _ in target_names {
        print!("---:|");
    }
    if target_names.len() >= 2 && target_names.first().map(String::as_str) == Some("bq-lite") {
        for _ in target_names.iter().skip(1) {
            print!("---:|");
        }
    }
    println!();

    for case in cases {
        let rows_str = target_names
            .iter()
            .find_map(|t| results.get(&case.name)?.get(t)?.rows)
            .map_or_else(|| "-".to_owned(), |n| n.to_string());
        print!("| `{}` | {} | {} |", case.name, case.category, rows_str);
        let bq_p50 = results
            .get(&case.name)
            .and_then(|m| m.get("bq-lite"))
            .and_then(|c| c.p50_ms);
        for t in target_names {
            let cell = match results.get(&case.name).and_then(|m| m.get(t)) {
                Some(m) if m.status == "ok" => {
                    format!("{:.2} ms", m.p50_ms.unwrap_or(0.0))
                }
                Some(_) => "ERR".to_owned(),
                None => "-".to_owned(),
            };
            print!(" {cell} |");
        }
        if target_names.len() >= 2 && target_names.first().map(String::as_str) == Some("bq-lite") {
            for other in target_names.iter().skip(1) {
                let other_p50 = results
                    .get(&case.name)
                    .and_then(|m| m.get(other))
                    .and_then(|c| c.p50_ms);
                let ratio = match (bq_p50, other_p50) {
                    (Some(b), Some(o)) if b > 0.0 => {
                        if o >= b {
                            format!("**{:.1}x faster**", o / b)
                        } else {
                            format!("{:.1}x slower", b / o)
                        }
                    }
                    _ => "-".to_owned(),
                };
                print!(" {ratio} |");
            }
        }
        println!();
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("Error: {msg}");
                return std::process::ExitCode::FAILURE;
            }
            return std::process::ExitCode::SUCCESS;
        }
    };
    let raw = match std::fs::read_to_string(&opts.cases_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", opts.cases_path.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let all_cases: Vec<BenchCase> = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse {}: {e}", opts.cases_path.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let cases: Vec<&BenchCase> = all_cases
        .iter()
        .filter(|c| opts.include_heavy || !c.heavy)
        .filter(|c| opts.filter.as_ref().is_none_or(|f| c.name.contains(f)))
        .collect();

    let client = match reqwest::Client::builder().pool_max_idle_per_host(4).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut targets = Vec::with_capacity(opts.targets.len());
    for name in &opts.targets {
        match start_target(name, opts.urls.get(name), &client).await {
            Ok(t) => targets.push(t),
            Err(e) => {
                eprintln!("Target startup failed: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let mut results: BTreeMap<String, BTreeMap<String, CaseMetric>> = BTreeMap::new();
    for case in &cases {
        let mut per_target = BTreeMap::new();
        for target in &targets {
            eprintln!("  [{:<10}] {}", target.name, case.name);
            let m = run_case_on_target(&client, target, case, opts.timeout_secs).await;
            if let Some(ref err) = m.error {
                eprintln!("    -> ERROR: {err}");
            }
            per_target.insert(target.name.clone(), m);
        }
        results.insert(case.name.clone(), per_target);
    }

    let target_names: Vec<String> = targets.iter().map(|t| t.name.clone()).collect();
    print_markdown_table(&cases, &target_names, &results);

    if let Some(ref out_path) = opts.out_json {
        if let Ok(json_str) = serde_json::to_string_pretty(&results) {
            let _ = std::fs::write(out_path, json_str);
            eprintln!("==> Wrote JSON results to {}", out_path.display());
        }
    }
    std::process::ExitCode::SUCCESS
}
