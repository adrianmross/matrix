use std::{
    env, fs,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    process::Command as ProcessCommand,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use reqwest::{Method, header::CONTENT_TYPE};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "matrix", version, about = "Compatibility matrix CLI")]
struct Cli {
    #[arg(long, env = "MATRIX_ORACLE_URL", global = true)]
    oracle: Option<String>,
    #[arg(long, env = "MATRIX_API_PREFIX", global = true)]
    api_prefix: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Config(ConfigCommand),
    List,
    View {
        zone: String,
    },
    Current(CandidateArgs),
    Gate {
        #[arg(long)]
        zone: String,
        #[arg(long, default_value = "preview")]
        level: String,
    },
    Trace(TraceArgs),
    Upload(UploadArgs),
    Publish(UploadArgs),
    Ingest(IngestArgs),
    Query {
        sql: String,
        #[arg(long, default_value_t = 1000)]
        max_facts: usize,
    },
    Enter,
    Doctor,
    RedPill,
    BluePill,
}

#[derive(Args)]
struct ConfigCommand {
    #[command(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Subcommand)]
enum ConfigSubcommand {
    List,
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Args, Clone)]
struct CandidateArgs {
    #[arg(long)]
    zone: String,
    #[arg(long, default_value = "preview")]
    level: String,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    sha: Option<String>,
    #[arg(long)]
    r#ref: Option<String>,
    #[arg(long)]
    capability: Option<String>,
}

#[derive(Args)]
struct TraceArgs {
    #[arg(long)]
    zone: Option<String>,
    #[arg(long)]
    subject: Option<String>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Args, Clone)]
struct UploadArgs {
    file: Option<PathBuf>,
    #[arg(long)]
    stdin: bool,
}

#[derive(Args)]
struct IngestArgs {
    adapter: String,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    stdin: bool,
    #[arg(long)]
    upload: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Config {
    oracle: Option<String>,
    api_prefix: Option<String>,
    token: Option<String>,
}

#[derive(Clone)]
struct Matrix {
    config_path: PathBuf,
    config: Config,
    oracle: Option<String>,
    api_prefix: String,
    json: bool,
    client: reqwest::Client,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut matrix = Matrix::load(cli.oracle, cli.api_prefix, cli.json)?;
    let output = match cli.command {
        Commands::Config(command) => config_command(&mut matrix, command).await?,
        Commands::List => matrix.get("").await?,
        Commands::View { zone } => matrix.get(&format!("/tracks/{}", enc(&zone))).await?,
        Commands::Current(args) => current(&matrix, args).await?,
        Commands::Gate { zone, level } => {
            matrix
                .get(&format!(
                    "/tracks/{}/promotion-gates/{}",
                    enc(&zone),
                    enc(&level)
                ))
                .await?
        }
        Commands::Trace(args) => trace(&matrix, args).await?,
        Commands::Upload(args) | Commands::Publish(args) => upload(&matrix, args).await?,
        Commands::Ingest(args) => ingest(&matrix, args).await?,
        Commands::Query { sql, max_facts } => query(&matrix, &sql, max_facts).await?,
        Commands::Enter => enter(&matrix).await?,
        Commands::Doctor => doctor(&matrix).await?,
        Commands::RedPill => red_pill(&matrix).await?,
        Commands::BluePill => blue_pill(&matrix).await?,
    };
    print_value(&output, matrix.json)?;
    Ok(())
}

impl Matrix {
    fn load(
        oracle_override: Option<String>,
        prefix_override: Option<String>,
        json: bool,
    ) -> Result<Self> {
        let config_path = config_path()?;
        let config = if config_path.exists() {
            serde_json::from_slice(&fs::read(&config_path)?)
                .with_context(|| format!("failed to parse {}", config_path.display()))?
        } else {
            Config::default()
        };
        let oracle = oracle_override
            .or_else(|| env::var("MATRIX_ORACLE_URL").ok())
            .or_else(|| config.oracle.clone())
            .map(|value| value.trim_end_matches('/').to_string());
        let api_prefix = prefix_override
            .or_else(|| env::var("MATRIX_API_PREFIX").ok())
            .or_else(|| config.api_prefix.clone())
            .unwrap_or_else(|| "/v1/compatibility".to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            config_path,
            config,
            oracle,
            api_prefix,
            json,
            client: reqwest::Client::builder()
                .user_agent(concat!("matrix/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.config_path, serde_json::to_vec_pretty(&self.config)?)?;
        Ok(())
    }

    fn oracle(&self) -> Result<&str> {
        self.oracle
            .as_deref()
            .ok_or_else(|| anyhow!("no oracle configured; run `matrix config set oracle <url>` or set MATRIX_ORACLE_URL"))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}{}", self.oracle()?, self.api_prefix, path);
        let mut request = self.client.request(method, &url);
        if let Some(token) = env::var("MATRIX_TOKEN")
            .ok()
            .or_else(|| self.config.token.clone())
        {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("request failed: {url}"))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = response.text().await?;
        if status.is_success() && looks_like_html(&content_type, &text) {
            bail!("received HTML from the oracle; authenticate or use a machine/API oracle URL");
        }
        let value = parse_response_text(&text);
        if !status.is_success() {
            bail!("oracle {status}: {}", error_detail(&value, &text));
        }
        Ok(value)
    }
}

async fn config_command(matrix: &mut Matrix, command: ConfigCommand) -> Result<Value> {
    match command.command {
        ConfigSubcommand::List => Ok(json!({
            "configPath": matrix.config_path,
            "oracle": matrix.config.oracle,
            "apiPrefix": matrix.config.api_prefix,
            "hasToken": matrix.config.token.is_some(),
        })),
        ConfigSubcommand::Get { key } => match key.as_str() {
            "oracle" => Ok(json!({"oracle": matrix.config.oracle})),
            "api-prefix" | "apiPrefix" => Ok(json!({"apiPrefix": matrix.config.api_prefix})),
            "token" => Ok(json!({"hasToken": matrix.config.token.is_some()})),
            _ => bail!("unknown config key {key:?}; expected oracle, api-prefix, or token"),
        },
        ConfigSubcommand::Set { key, value } => {
            match key.as_str() {
                "oracle" => matrix.config.oracle = Some(value),
                "api-prefix" | "apiPrefix" => matrix.config.api_prefix = Some(value),
                "token" => matrix.config.token = Some(value),
                _ => bail!("unknown config key {key:?}; expected oracle, api-prefix, or token"),
            }
            matrix.save()?;
            Ok(json!({"saved": matrix.config_path}))
        }
    }
}

async fn current(matrix: &Matrix, args: CandidateArgs) -> Result<Value> {
    let repo = args
        .repo
        .or_else(current_repo)
        .ok_or_else(|| anyhow!("--repo is required when git remote origin cannot be detected"))?;
    let sha = args.sha.or_else(current_sha);
    let reference = args
        .r#ref
        .or(args.tag)
        .or_else(current_exact_tag)
        .or_else(current_branch);
    let mut query = vec![("repo", repo)];
    if let Some(sha) = sha {
        query.push(("sha", sha));
    }
    if let Some(reference) = reference {
        query.push(("ref", reference));
    }
    if let Some(capability) = args.capability {
        query.push(("capability", capability));
    }
    matrix
        .get(&format!(
            "/tracks/{}/promotion-candidates/{}?{}",
            enc(&args.zone),
            enc(&args.level),
            query_string(query)
        ))
        .await
}

async fn trace(matrix: &Matrix, args: TraceArgs) -> Result<Value> {
    let mut query = Vec::new();
    if let Some(zone) = args.zone {
        query.push(("track", zone));
    }
    if let Some(subject) = args.subject {
        query.push(("subjectName", subject));
    }
    if let Some(id) = args.id {
        query.push(("id", id));
    }
    query.push(("limit", args.limit.to_string()));
    let facts = matrix
        .get(&format!("/facts?{}", query_string(query)))
        .await?;
    Ok(json!({
        "trace": facts,
        "hint": "Use matrix red-pill for a deeper diagnostic bundle."
    }))
}

async fn upload(matrix: &Matrix, args: UploadArgs) -> Result<Value> {
    let body = read_input(args.file, args.stdin)?;
    matrix.request(Method::POST, "/facts", Some(body)).await
}

async fn ingest(matrix: &Matrix, args: IngestArgs) -> Result<Value> {
    let payload = read_input(args.file, args.stdin)?;
    let fact = json!({
        "adapter": args.adapter,
        "format": "matrix.ingest.v1",
        "payload": payload,
    });
    if args.upload {
        matrix.request(Method::POST, "/facts", Some(fact)).await
    } else {
        Ok(fact)
    }
}

async fn query(matrix: &Matrix, sql: &str, max_facts: usize) -> Result<Value> {
    let normalized = sql.trim().to_ascii_lowercase();
    if !(normalized.starts_with("select ") || normalized.starts_with("with ")) {
        bail!("matrix query only allows read-only SELECT/WITH statements");
    }
    let facts = fetch_facts(matrix, max_facts).await?;
    let db = Connection::open_in_memory()?;
    db.execute_batch(
        "create table facts (
          id text, zone text, kind text, status text,
          source_repository text, source_sha text,
          subject_type text, subject_name text, channel text,
          observed_at text, json text not null
        );",
    )?;
    for fact in facts {
        db.execute(
            "insert into facts values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                fact.get("id").and_then(Value::as_str),
                fact.get("track")
                    .or_else(|| fact.get("zone"))
                    .and_then(Value::as_str),
                fact.get("kind").and_then(Value::as_str),
                fact.get("status").and_then(Value::as_str),
                fact.get("sourceRepository").and_then(Value::as_str),
                fact.get("sourceSha").and_then(Value::as_str),
                fact.get("subjectType").and_then(Value::as_str),
                fact.get("subjectName").and_then(Value::as_str),
                fact.get("channel").and_then(Value::as_str),
                fact.get("observedAt").and_then(Value::as_str),
                serde_json::to_string(&fact)?,
            ],
        )?;
    }
    let mut stmt = db.prepare(sql)?;
    let columns: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let rows = stmt
        .query_map([], |row| {
            let mut object = serde_json::Map::new();
            for (index, column) in columns.iter().enumerate() {
                object.insert(column.clone(), sqlite_value_to_json(row.get_ref(index)?));
            }
            Ok(Value::Object(object))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!({ "columns": columns, "rows": rows }))
}

async fn enter(matrix: &Matrix) -> Result<Value> {
    eprintln!("Matrix shell. Type SQL, `red` to exit, `blue` to clear, `help` for commands.");
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    loop {
        write!(stdout, "matrix> ")?;
        stdout.flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line {
            "red" | "red-pill" | "exit" | "quit" => {
                eprintln!("Wake up.");
                break;
            }
            "blue" | "blue-pill" | "clear" => {
                eprintln!("Session context cleared.");
                continue;
            }
            "help" => {
                eprintln!("SQL: select ... from facts; commands: red, blue, help");
                continue;
            }
            _ => match query(matrix, line, 1000).await {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
                Err(error) => eprintln!("{error:#}"),
            },
        }
    }
    Ok(json!({"status":"left"}))
}

async fn doctor(matrix: &Matrix) -> Result<Value> {
    let oracle = matrix.oracle.clone();
    let reachable = if oracle.is_some() {
        matrix.get("").await.map(|_| true).unwrap_or(false)
    } else {
        false
    };
    Ok(json!({
        "configPath": matrix.config_path,
        "oracle": oracle,
        "apiPrefix": matrix.api_prefix,
        "reachable": reachable,
    }))
}

async fn red_pill(matrix: &Matrix) -> Result<Value> {
    let overview = matrix.get("").await.ok();
    Ok(json!({
        "mode": "red-pill",
        "message": "Follow the trace.",
        "doctor": doctor(matrix).await?,
        "overview": overview,
    }))
}

async fn blue_pill(matrix: &Matrix) -> Result<Value> {
    Ok(json!({
        "mode": "blue-pill",
        "message": "The story ends here.",
        "doctor": doctor(matrix).await?,
    }))
}

async fn fetch_facts(matrix: &Matrix, max_facts: usize) -> Result<Vec<Value>> {
    let mut facts = Vec::new();
    let mut cursor: Option<String> = None;
    while facts.len() < max_facts {
        let limit = (max_facts - facts.len()).min(200);
        let mut query = vec![("limit", limit.to_string())];
        if let Some(cursor) = cursor.clone() {
            query.push(("cursor", cursor));
        }
        let body = matrix
            .get(&format!("/facts?{}", query_string(query)))
            .await?;
        facts.extend(body["facts"].as_array().cloned().unwrap_or_default());
        cursor = body["page"]["nextCursor"].as_str().map(ToString::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(facts)
}

fn read_input(file: Option<PathBuf>, stdin: bool) -> Result<Value> {
    if stdin {
        return read_stdin_json();
    }
    if let Some(file) = file {
        return Ok(serde_json::from_slice(&fs::read(file)?)?);
    }
    bail!("provide a file path or --stdin")
}

fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "matrix", "matrix")
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dirs.config_dir().join("config.json"))
}

fn read_stdin_string() -> Result<String> {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("stdin was empty");
    }
    Ok(text)
}

fn read_stdin_json() -> Result<Value> {
    serde_json::from_str(&read_stdin_string()?).context("stdin was not valid JSON")
}

fn parse_response_text(text: &str) -> Value {
    if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
    }
}

fn looks_like_html(content_type: &str, text: &str) -> bool {
    content_type.contains("text/html") || text.trim_start().starts_with("<!DOCTYPE html")
}

fn error_detail(value: &Value, fallback: &str) -> String {
    value
        .get("error")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            if value.is_null() {
                fallback.to_string()
            } else {
                value.to_string()
            }
        })
}

fn sqlite_value_to_json(value: rusqlite::types::ValueRef<'_>) -> Value {
    match value {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(value) => json!(value),
        rusqlite::types::ValueRef::Real(value) => json!(value),
        rusqlite::types::ValueRef::Text(value) => {
            Value::String(String::from_utf8_lossy(value).to_string())
        }
        rusqlite::types::ValueRef::Blob(value) => Value::String(format!("{value:?}")),
    }
}

fn print_value(value: &Value, json_output: bool) -> Result<()> {
    if json_output || !value.is_string() {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else if let Some(text) = value.as_str() {
        println!("{text}");
    }
    Ok(())
}

fn query_string(values: Vec<(&str, String)>) -> String {
    values
        .into_iter()
        .map(|(key, value)| format!("{}={}", enc(key), enc(&value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn enc(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

fn current_repo() -> Option<String> {
    let remote = command_output("git", &["remote", "get-url", "origin"])?;
    normalize_repo_url(remote.trim())
}

fn normalize_repo_url(value: &str) -> Option<String> {
    let mut text = value.trim().trim_end_matches(".git").to_string();
    if let Some(index) = text.find("github.com:") {
        text = text[index + "github.com:".len()..].to_string();
    } else if let Some(index) = text.find("github.com/") {
        text = text[index + "github.com/".len()..].to_string();
    }
    if text.contains('/') { Some(text) } else { None }
}

fn current_sha() -> Option<String> {
    command_output("git", &["rev-parse", "HEAD"]).map(|value| value.trim().to_string())
}

fn current_exact_tag() -> Option<String> {
    command_output("git", &["describe", "--tags", "--exact-match"])
        .map(|value| value.trim().to_string())
}

fn current_branch() -> Option<String> {
    command_output("git", &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|value| value.trim().to_string())
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new(command).args(args).output().ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_github_repos() {
        assert_eq!(
            normalize_repo_url("git@github.com:example/project.git").as_deref(),
            Some("example/project")
        );
        assert_eq!(
            normalize_repo_url("https://github.com/example/project.git").as_deref(),
            Some("example/project")
        );
    }

    #[test]
    fn builds_query_strings() {
        assert_eq!(
            query_string(vec![
                ("repo", "example/project".to_string()),
                ("level", "preview".to_string())
            ]),
            "repo=example%2Fproject&level=preview"
        );
    }
}
