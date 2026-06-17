use std::{
    env, fs,
    io::{self, Read},
    path::PathBuf,
    process::Command as ProcessCommand,
    time::SystemTime,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use comfy_table::{Table, presets::UTF8_FULL};
use directories::ProjectDirs;
use nu_ansi_term::{Color, Style};
use reedline::{
    ColumnarMenu, Completer, DefaultHinter, DefaultPrompt, DefaultPromptSegment, FileBackedHistory,
    Highlighter, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, StyledText,
    Suggestion, default_emacs_keybindings,
};
use reqwest::{Method, header::CONTENT_TYPE};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "matrix", version, about = "Compatibility matrix CLI")]
struct Cli {
    #[arg(long, env = "MATRIX_CONSTRUCT_URL", global = true)]
    construct: Option<String>,
    #[arg(long, env = "MATRIX_API_PREFIX", global = true)]
    api_prefix: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Completion {
        shell: Shell,
    },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    construct: Option<String>,
    api_prefix: Option<String>,
    token: Option<String>,
}

#[derive(Clone)]
struct Matrix {
    config_path: PathBuf,
    config: Config,
    construct: Option<String>,
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
    if let Commands::Completion { shell } = cli.command {
        let mut command = Cli::command();
        generate(shell, &mut command, "matrix", &mut io::stdout());
        return Ok(());
    }
    let mut matrix = Matrix::load(cli.construct, cli.api_prefix, cli.json)?;
    let output = match cli.command {
        Commands::Completion { .. } => unreachable!("completion exits before matrix is loaded"),
        Commands::Config(command) => config_command(&mut matrix, command).await?,
        Commands::List => matrix.get("").await?,
        Commands::View { zone } => {
            matrix
                .get_fallback(
                    &format!("/zones/{}", enc(&zone)),
                    &format!("/tracks/{}", enc(&zone)),
                )
                .await?
        }
        Commands::Current(args) => current(&matrix, args).await?,
        Commands::Gate { zone, level } => {
            matrix
                .get_fallback(
                    &format!("/zones/{}/gates/{}", enc(&zone), enc(&level)),
                    &format!("/tracks/{}/promotion-gates/{}", enc(&zone), enc(&level)),
                )
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
        construct_override: Option<String>,
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
        let construct = construct_override
            .or_else(|| env::var("MATRIX_CONSTRUCT_URL").ok())
            .or_else(|| config.construct.clone())
            .map(|value| value.trim_end_matches('/').to_string());
        let api_prefix = prefix_override
            .or_else(|| env::var("MATRIX_API_PREFIX").ok())
            .or_else(|| config.api_prefix.clone())
            .unwrap_or_else(|| "/v1/matrix".to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            config_path,
            config,
            construct,
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

    fn construct(&self) -> Result<&str> {
        self.construct
            .as_deref()
            .ok_or_else(|| anyhow!("no construct configured; run `matrix config set construct <url>` or set MATRIX_CONSTRUCT_URL"))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    async fn get_fallback(&self, primary: &str, fallback: &str) -> Result<Value> {
        match self.request(Method::GET, primary, None).await {
            Ok(value) => Ok(value),
            Err(primary_error) => self
                .request(Method::GET, fallback, None)
                .await
                .with_context(|| format!("primary path failed: {primary_error:#}")),
        }
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}{}", self.construct()?, self.api_prefix, path);
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
            bail!(
                "received HTML from the construct; authenticate or use a machine/API construct URL"
            );
        }
        let value = parse_response_text(&text);
        if !status.is_success() {
            bail!("construct {status}: {}", error_detail(&value, &text));
        }
        Ok(value)
    }
}

async fn config_command(matrix: &mut Matrix, command: ConfigCommand) -> Result<Value> {
    match command.command {
        ConfigSubcommand::List => Ok(json!({
            "configPath": matrix.config_path,
            "construct": matrix.config.construct,
            "apiPrefix": matrix.config.api_prefix,
            "hasToken": matrix.config.token.is_some(),
        })),
        ConfigSubcommand::Get { key } => match key.as_str() {
            "construct" => Ok(json!({"construct": matrix.config.construct})),
            "api-prefix" | "apiPrefix" => Ok(json!({"apiPrefix": matrix.config.api_prefix})),
            "token" => Ok(json!({"hasToken": matrix.config.token.is_some()})),
            _ => bail!("unknown config key {key:?}; expected construct, api-prefix, or token"),
        },
        ConfigSubcommand::Set { key, value } => {
            match key.as_str() {
                "construct" => matrix.config.construct = Some(value),
                "api-prefix" | "apiPrefix" => matrix.config.api_prefix = Some(value),
                "token" => matrix.config.token = Some(value),
                _ => bail!("unknown config key {key:?}; expected construct, api-prefix, or token"),
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
    let qs = query_string(query);
    matrix
        .get_fallback(
            &format!(
                "/zones/{}/candidates/{}?{}",
                enc(&args.zone),
                enc(&args.level),
                qs
            ),
            &format!(
                "/tracks/{}/promotion-candidates/{}?{}",
                enc(&args.zone),
                enc(&args.level),
                qs
            ),
        )
        .await
}

async fn trace(matrix: &Matrix, args: TraceArgs) -> Result<Value> {
    let mut query = Vec::new();
    let mut fallback_query = Vec::new();
    if let Some(zone) = args.zone {
        query.push(("zone", zone.clone()));
        fallback_query.push(("track", zone));
    }
    if let Some(subject) = args.subject {
        query.push(("subjectName", subject.clone()));
        fallback_query.push(("subjectName", subject));
    }
    if let Some(id) = args.id {
        query.push(("id", id.clone()));
        fallback_query.push(("id", id));
    }
    query.push(("limit", args.limit.to_string()));
    fallback_query.push(("limit", args.limit.to_string()));
    let facts = matrix
        .get_fallback(
            &format!("/facts?{}", query_string(query)),
            &format!("/facts?{}", query_string(fallback_query)),
        )
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
    let facts = fetch_facts(matrix, max_facts).await?;
    let db = build_facts_db(&facts)?;
    execute_readonly_sql(&db, sql)
}

async fn enter(matrix: &Matrix) -> Result<Value> {
    let mut repl = ReplSession::new(matrix).await?;
    repl.run().await?;
    Ok(json!({"status":"left"}))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Table,
    Json,
    Csv,
}

struct ReplSession<'a> {
    matrix: &'a Matrix,
    db: Connection,
    max_facts: usize,
    fact_count: usize,
    output_mode: OutputMode,
    expanded: bool,
    timing: bool,
    last_refresh: SystemTime,
}

impl<'a> ReplSession<'a> {
    async fn new(matrix: &'a Matrix) -> Result<Self> {
        let max_facts = 1000;
        let facts = fetch_facts(matrix, max_facts).await?;
        let fact_count = facts.len();
        Ok(Self {
            matrix,
            db: build_facts_db(&facts)?,
            max_facts,
            fact_count,
            output_mode: if matrix.json {
                OutputMode::Json
            } else {
                OutputMode::Table
            },
            expanded: false,
            timing: false,
            last_refresh: SystemTime::now(),
        })
    }

    async fn refresh(&mut self) -> Result<()> {
        let facts = fetch_facts(self.matrix, self.max_facts).await?;
        self.fact_count = facts.len();
        self.db = build_facts_db(&facts)?;
        self.last_refresh = SystemTime::now();
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        eprintln!(
            "Matrix shell. Type SQL ending in `;`, `.help` for commands, `red` to exit, `blue` to clear."
        );
        eprintln!("Loaded {} facts into the local session.", self.fact_count);

        let history_path = repl_history_path()?;
        if let Some(parent) = history_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let history = Box::new(FileBackedHistory::with_file(5000, history_path)?);
        let completion_menu = Box::new(ColumnarMenu::default().with_name("completion_menu"));
        let mut keybindings = default_emacs_keybindings();
        keybindings.add_binding(
            reedline::KeyModifiers::NONE,
            reedline::KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]),
        );
        let edit_mode = Box::new(reedline::Emacs::new(keybindings));
        let mut line_editor = Reedline::create()
            .with_history(history)
            .with_completer(Box::new(MatrixCompleter::new()))
            .with_highlighter(Box::new(MatrixHighlighter))
            .with_hinter(Box::new(
                DefaultHinter::default().with_style(Style::new().italic().fg(Color::LightGray)),
            ))
            .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
            .with_edit_mode(edit_mode);

        let prompt = DefaultPrompt::new(
            DefaultPromptSegment::Basic("matrix".to_string()),
            DefaultPromptSegment::Empty,
        );
        let continuation_prompt = DefaultPrompt::new(
            DefaultPromptSegment::Basic("...".to_string()),
            DefaultPromptSegment::Empty,
        );
        let mut buffer = String::new();

        loop {
            let active_prompt = if buffer.trim().is_empty() {
                &prompt
            } else {
                &continuation_prompt
            };
            match line_editor.read_line(active_prompt) {
                Ok(Signal::Success(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    if buffer.trim().is_empty() && is_repl_command(line) {
                        if self.handle_command(line).await? {
                            break;
                        }
                        continue;
                    }

                    if !buffer.is_empty() {
                        buffer.push('\n');
                    }
                    buffer.push_str(line);

                    if is_complete_sql(&buffer) {
                        let sql = buffer.trim().trim_end_matches(';').trim().to_string();
                        buffer.clear();
                        self.run_sql(&sql)?;
                    }
                }
                Ok(Signal::CtrlD) => break,
                Ok(Signal::CtrlC) => {
                    if buffer.is_empty() {
                        eprintln!("Use `red` to exit.");
                    } else {
                        buffer.clear();
                        eprintln!("Cleared query buffer.");
                    }
                }
                Ok(Signal::HostCommand(command)) => {
                    if self.handle_command(&command).await? {
                        break;
                    }
                }
                Ok(Signal::ExternalBreak(_)) => break,
                Ok(_) => continue,
                Err(error) => return Err(error.into()),
            }
        }

        eprintln!("Wake up.");
        Ok(())
    }

    fn run_sql(&self, sql: &str) -> Result<()> {
        let start = SystemTime::now();
        let result = execute_readonly_sql(&self.db, sql)?;
        print_query_result(&result, self.output_mode, self.expanded)?;
        if self.timing
            && let Ok(elapsed) = start.elapsed()
        {
            eprintln!("Time: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
        }
        Ok(())
    }

    async fn handle_command(&mut self, raw: &str) -> Result<bool> {
        let command = raw.trim();
        let command = command
            .strip_prefix('/')
            .or_else(|| command.strip_prefix('.'))
            .unwrap_or(command)
            .trim();
        let mut parts = command.split_whitespace();
        let Some(name) = parts.next() else {
            return Ok(false);
        };

        match name {
            "red" | "red-pill" | "exit" | "quit" | "q" => return Ok(true),
            "blue" | "blue-pill" | "clear" => {
                eprintln!("Session context cleared.");
            }
            "help" | "?" => print_repl_help(),
            "status" => self.print_status()?,
            "tables" => self.run_sql(
                "select name from sqlite_master where type in ('table', 'view') order by name",
            )?,
            "schema" => {
                let table = parts.next();
                if let Some(table) = table {
                    self.run_sql(&format!(
                        "select sql from sqlite_master where name = '{}'",
                        table.replace('\'', "''")
                    ))?;
                } else {
                    self.run_sql("select name, type, sql from sqlite_master where sql is not null order by type, name")?;
                }
            }
            "describe" | "desc" | "d" => {
                let table = parts.next().unwrap_or("facts");
                self.run_sql(&format!(
                    "select * from pragma_table_info('{}')",
                    table.replace('\'', "''")
                ))?;
            }
            "mode" => match parts.next() {
                Some("table") | Some("aligned") => {
                    self.output_mode = OutputMode::Table;
                    eprintln!("Output mode: table");
                }
                Some("json") => {
                    self.output_mode = OutputMode::Json;
                    eprintln!("Output mode: json");
                }
                Some("csv") => {
                    self.output_mode = OutputMode::Csv;
                    eprintln!("Output mode: csv");
                }
                Some(other) => eprintln!("Unknown mode {other:?}; expected table, json, or csv."),
                None => eprintln!("Output mode: {:?}", self.output_mode),
            },
            "x" | "expanded" => {
                self.expanded = !self.expanded;
                eprintln!(
                    "Expanded output: {}",
                    if self.expanded { "on" } else { "off" }
                );
            }
            "timing" => {
                self.timing = !self.timing;
                eprintln!("Timing: {}", if self.timing { "on" } else { "off" });
            }
            "limit" => match parts.next().and_then(|value| value.parse::<usize>().ok()) {
                Some(limit) if limit > 0 => {
                    self.max_facts = limit;
                    self.refresh().await?;
                    eprintln!(
                        "Fact limit: {}; loaded {} facts.",
                        self.max_facts, self.fact_count
                    );
                }
                _ => eprintln!("Usage: .limit 2000"),
            },
            "refresh" => {
                self.refresh().await?;
                eprintln!("Refreshed {} facts.", self.fact_count);
            }
            "zones" => self.run_sql("select * from zones order by zone")?,
            "subjects" => self.run_sql("select * from subjects order by subject_name")?,
            "trace" => {
                let subject = parts.collect::<Vec<_>>().join(" ");
                if subject.is_empty() {
                    eprintln!("Usage: .trace <subject-name>");
                } else {
                    self.run_sql(&format!(
                        "select id, zone, kind, status, subject_name, observed_at from facts where subject_name = '{}' order by observed_at desc",
                        subject.replace('\'', "''")
                    ))?;
                }
            }
            "gate" => {
                let zone = parts.next();
                let level = parts.next().unwrap_or("preview");
                if let Some(zone) = zone {
                    let value = self
                        .matrix
                        .get_fallback(
                            &format!("/zones/{}/gates/{}", enc(zone), enc(level)),
                            &format!("/tracks/{}/promotion-gates/{}", enc(zone), enc(level)),
                        )
                        .await?;
                    print_value(&value, self.matrix.json)?;
                } else {
                    eprintln!("Usage: .gate <zone> [level]");
                }
            }
            "explain" => {
                let sql = parts.collect::<Vec<_>>().join(" ");
                if sql.is_empty() {
                    eprintln!("Usage: .explain select ...");
                } else {
                    self.run_sql(&format!("explain query plan {sql}"))?;
                }
            }
            other => eprintln!("Unknown command {other:?}. Try `.help`."),
        }

        Ok(false)
    }

    fn print_status(&self) -> Result<()> {
        let refreshed = self
            .last_refresh
            .elapsed()
            .map(|elapsed| format!("{}s ago", elapsed.as_secs()))
            .unwrap_or_else(|_| "unknown".to_string());
        let value = json!({
            "construct": self.matrix.construct,
            "apiPrefix": self.matrix.api_prefix,
            "facts": self.fact_count,
            "maxFacts": self.max_facts,
            "mode": format!("{:?}", self.output_mode).to_ascii_lowercase(),
            "expanded": self.expanded,
            "timing": self.timing,
            "refreshed": refreshed,
        });
        print_value(&value, self.matrix.json)
    }
}

fn build_facts_db(facts: &[Value]) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(
        "create table facts (
          id text, zone text, kind text, status text,
          source_repository text, source_sha text,
          subject_type text, subject_name text, channel text,
          observed_at text, json text not null
        );
        create view zones as
          select zone, count(*) as facts,
                 sum(case when status = 'compatible' then 1 else 0 end) as compatible,
                 sum(case when status = 'incompatible' then 1 else 0 end) as incompatible
          from facts
          where zone is not null
          group by zone;
        create view subjects as
          select subject_type, subject_name, count(*) as facts,
                 max(observed_at) as last_observed_at
          from facts
          where subject_name is not null
          group by subject_type, subject_name;",
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
                serde_json::to_string(fact)?,
            ],
        )?;
    }
    Ok(db)
}

fn execute_readonly_sql(db: &Connection, sql: &str) -> Result<Value> {
    let normalized = sql.trim().to_ascii_lowercase();
    if !(normalized.starts_with("select ")
        || normalized.starts_with("with ")
        || normalized.starts_with("explain query plan "))
    {
        bail!("matrix query only allows read-only SELECT/WITH/EXPLAIN statements");
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

fn print_query_result(value: &Value, mode: OutputMode, expanded: bool) -> Result<()> {
    match mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(value)?),
        OutputMode::Csv => print_csv_result(value)?,
        OutputMode::Table => print_table_result(value, expanded)?,
    }
    Ok(())
}

fn print_table_result(value: &Value, expanded: bool) -> Result<()> {
    let columns = value["columns"].as_array().cloned().unwrap_or_default();
    let rows = value["rows"].as_array().cloned().unwrap_or_default();
    let column_names = columns
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    if expanded {
        for (index, row) in rows.iter().enumerate() {
            println!("-[ RECORD {} ]-------------------------", index + 1);
            for column in &column_names {
                println!(
                    "{column:<20} {}",
                    display_cell(row.get(column).unwrap_or(&Value::Null))
                );
            }
        }
        println!("({} rows)", rows.len());
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(column_names.clone());
    for row in &rows {
        table.add_row(
            column_names
                .iter()
                .map(|column| display_cell(row.get(column).unwrap_or(&Value::Null)))
                .collect::<Vec<_>>(),
        );
    }
    println!("{table}");
    println!("({} rows)", rows.len());
    Ok(())
}

fn print_csv_result(value: &Value) -> Result<()> {
    let columns = value["columns"].as_array().cloned().unwrap_or_default();
    let rows = value["rows"].as_array().cloned().unwrap_or_default();
    let column_names = columns.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    println!(
        "{}",
        column_names
            .iter()
            .map(|column| csv_escape(column))
            .collect::<Vec<_>>()
            .join(",")
    );
    for row in rows {
        println!(
            "{}",
            column_names
                .iter()
                .map(|column| csv_escape(&display_cell(row.get(*column).unwrap_or(&Value::Null))))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    Ok(())
}

fn display_cell(value: &Value) -> String {
    match value {
        Value::Null => "".to_string(),
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn print_repl_help() {
    eprintln!(
        "\
Matrix shell commands
  .help, /help              Show this help
  .status, /status          Show session, construct, cache, and output state
  .tables                   List local tables and views
  .schema [table]           Show local SQL schema
  .describe [table]         Show columns for a table or view
  .mode table|json|csv      Change output format
  .x                        Toggle expanded table output
  .timing                   Toggle query timing
  .limit <n>                Set fact fetch limit and refresh cache
  .refresh                  Reload facts from the construct
  .zones                    Summarize facts by zone
  .subjects                 Summarize facts by subject
  .trace <subject>          Show recent facts for a subject
  .gate <zone> [level]      Fetch a gate decision from the construct
  .explain <sql>            Run EXPLAIN QUERY PLAN
  red, red-pill, .exit      Exit
  blue, blue-pill           Clear the current session context

SQL
  End SQL statements with `;`.
  Available tables/views: facts, zones, subjects.
"
    );
}

fn is_repl_command(line: &str) -> bool {
    line.starts_with('.')
        || line.starts_with('/')
        || matches!(
            line,
            "red" | "red-pill" | "blue" | "blue-pill" | "exit" | "quit" | "help"
        )
}

fn is_complete_sql(sql: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut previous = '\0';
    for current in sql.chars() {
        match current {
            '\'' if !in_double && previous != '\\' => in_single = !in_single,
            '"' if !in_single && previous != '\\' => in_double = !in_double,
            ';' if !in_single && !in_double => return true,
            _ => {}
        }
        previous = current;
    }
    false
}

fn repl_history_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "matrix", "matrix")
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dirs.data_dir().join("repl-history.txt"))
}

struct MatrixCompleter {
    words: Vec<String>,
}

impl MatrixCompleter {
    fn new() -> Self {
        let words = [
            ".describe",
            ".explain",
            ".gate",
            ".help",
            ".limit",
            ".mode",
            ".refresh",
            ".schema",
            ".status",
            ".subjects",
            ".tables",
            ".timing",
            ".trace",
            ".zones",
            "/help",
            "/status",
            "blue",
            "by",
            "channel",
            "compatible",
            "count",
            "csv",
            "facts",
            "from",
            "group",
            "id",
            "incompatible",
            "json",
            "kind",
            "limit",
            "observed_at",
            "order",
            "red",
            "select",
            "source_repository",
            "source_sha",
            "status",
            "subject_name",
            "subject_type",
            "subjects",
            "table",
            "where",
            "with",
            "zone",
            "zones",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect();
        Self { words }
    }
}

impl Completer for MatrixCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let safe_pos = pos.min(line.len());
        let start = line[..safe_pos]
            .rfind(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '(' | ')')
            })
            .map(|index| index + 1)
            .unwrap_or(0);
        let prefix = &line[start..safe_pos].to_ascii_lowercase();
        self.words
            .iter()
            .filter(|word| word.starts_with(prefix))
            .map(|word| Suggestion {
                value: word.clone(),
                span: Span::new(start, safe_pos),
                append_whitespace: !word.starts_with('.')
                    && !word.starts_with('/')
                    && !matches!(word.as_str(), "facts" | "zones" | "subjects"),
                ..Suggestion::default()
            })
            .collect()
    }
}

struct MatrixHighlighter;

impl Highlighter for MatrixHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut text = StyledText::new();
        let mut start = 0;
        for (index, token) in line
            .split_inclusive(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '(' | ')' | ';')
            })
            .enumerate()
        {
            let style = token_style(token.trim_matches(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '(' | ')' | ';')
            }));
            let _ = index;
            text.push((style, line[start..start + token.len()].to_string()));
            start += token.len();
        }
        if start < line.len() {
            let token = &line[start..];
            text.push((token_style(token), token.to_string()));
        }
        text
    }

    fn is_inside_string_literal(&self, line: &str, cursor: usize) -> bool {
        let mut in_single = false;
        for current in line[..cursor.min(line.len())].chars() {
            if current == '\'' {
                in_single = !in_single;
            }
        }
        in_single
    }
}

fn token_style(token: &str) -> Style {
    let lower = token.to_ascii_lowercase();
    if token.starts_with('.') || token.starts_with('/') {
        Style::new().fg(Color::Cyan).bold()
    } else if token.starts_with('\'') || token.starts_with('"') {
        Style::new().fg(Color::Green)
    } else if matches!(
        lower.as_str(),
        "select"
            | "with"
            | "from"
            | "where"
            | "group"
            | "by"
            | "order"
            | "limit"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "on"
            | "as"
            | "and"
            | "or"
            | "not"
            | "null"
            | "is"
            | "like"
            | "in"
            | "case"
            | "when"
            | "then"
            | "else"
            | "end"
            | "explain"
            | "pragma"
    ) {
        Style::new().fg(Color::Purple).bold()
    } else if matches!(lower.as_str(), "facts" | "zones" | "subjects") {
        Style::new().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

async fn doctor(matrix: &Matrix) -> Result<Value> {
    let construct = matrix.construct.clone();
    let reachable = if construct.is_some() {
        matrix.get("").await.map(|_| true).unwrap_or(false)
    } else {
        false
    };
    Ok(json!({
        "configPath": matrix.config_path,
        "construct": construct,
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
