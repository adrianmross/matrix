use std::{
    env, fs,
    io::{self, Read},
    path::PathBuf,
    process::Command as ProcessCommand,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use directories::ProjectDirs;
use reqwest::{Method, header::CONTENT_TYPE};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(feature = "interactive")]
use std::time::SystemTime;

#[cfg(feature = "interactive")]
use comfy_table::{Table, presets::UTF8_FULL};
#[cfg(feature = "interactive")]
use nu_ansi_term::{Color, Style};
#[cfg(feature = "interactive")]
use reedline::{
    ColumnarMenu, Completer, DefaultHinter, DefaultPrompt, DefaultPromptSegment, FileBackedHistory,
    Highlighter, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, StyledText,
    Suggestion, default_emacs_keybindings,
};

#[derive(Parser)]
#[command(name = "matrix", version, about = "Compatibility matrix CLI")]
struct Cli {
    #[arg(long, env = "MATRIX_CONSTRUCT_URL", global = true)]
    construct: Option<String>,
    #[arg(long, env = "MATRIX_API_PREFIX", global = true)]
    api_prefix: Option<String>,
    #[arg(short = 'o', long = "out", env = "MATRIX_OUTPUT", value_enum, global = true, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
    #[arg(long, global = true, hide = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
    Yaml,
    Table,
    Csv,
}

impl OutputFormat {
    fn from_cli(output: OutputFormat, json: bool) -> Self {
        if json { Self::Json } else { output }
    }
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
    Query(QueryArgs),
    Enter(ContextArgs),
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

#[derive(Args, Clone, Default)]
struct ContextArgs {
    #[arg(long, env = "MATRIX_ZONE")]
    zone: Option<String>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    component: Option<String>,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    sha: Option<String>,
    #[arg(long)]
    r#ref: Option<String>,
}

#[cfg(feature = "interactive")]
#[derive(Args, Clone, Default)]
struct EnterContextArgs {
    #[arg(long, env = "MATRIX_ZONE")]
    zone: Option<String>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    component: Option<String>,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long = "target-version", alias = "context-version")]
    target_version: Option<String>,
    #[arg(long)]
    sha: Option<String>,
    #[arg(long)]
    r#ref: Option<String>,
}

#[cfg(feature = "interactive")]
impl From<EnterContextArgs> for ContextArgs {
    fn from(args: EnterContextArgs) -> Self {
        Self {
            zone: args.zone,
            repo: args.repo,
            component: args.component,
            tag: args.tag,
            version: args.target_version,
            sha: args.sha,
            r#ref: args.r#ref,
        }
    }
}

#[derive(Args)]
struct QueryArgs {
    sql: String,
    #[arg(long, default_value_t = 1000)]
    max_facts: usize,
    #[command(flatten)]
    context: ContextArgs,
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
    output: OutputFormat,
    client: reqwest::Client,
}

#[derive(Clone, Debug, Default)]
struct MatrixContext {
    zone: Option<String>,
    repo: Option<String>,
    component: Option<String>,
    version: Option<String>,
    tag: Option<String>,
    sha: Option<String>,
    reference: Option<String>,
}

pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    if let Commands::Completion { shell } = cli.command {
        let mut command = Cli::command();
        generate(shell, &mut command, "matrix", &mut io::stdout());
        return Ok(());
    }
    let output_format = OutputFormat::from_cli(cli.output, cli.json);
    let mut matrix = Matrix::load(cli.construct, cli.api_prefix, output_format)?;
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
        Commands::Query(args) => query(&matrix, args).await?,
        Commands::Enter(context) => dispatch_enter(&matrix, context)?,
        Commands::Doctor => doctor(&matrix).await?,
        Commands::RedPill => red_pill(&matrix).await?,
        Commands::BluePill => blue_pill(&matrix).await?,
    };
    print_value(&output, matrix.output)?;
    Ok(())
}

#[cfg(feature = "interactive")]
pub async fn run_enter_cli() -> Result<()> {
    #[derive(Parser)]
    #[command(
        name = "matrix-enter",
        version,
        about = "Interactive SQL shell for Matrix"
    )]
    struct EnterCli {
        #[arg(long, env = "MATRIX_CONSTRUCT_URL")]
        construct: Option<String>,
        #[arg(long, env = "MATRIX_API_PREFIX")]
        api_prefix: Option<String>,
        #[arg(short = 'o', long = "out", env = "MATRIX_OUTPUT", value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
        #[arg(long, hide = true)]
        json: bool,
        #[command(flatten)]
        context: EnterContextArgs,
    }

    let cli = EnterCli::parse();
    let output_format = OutputFormat::from_cli(cli.output, cli.json);
    let matrix = Matrix::load(cli.construct, cli.api_prefix, output_format)?;
    let output = enter(&matrix, cli.context.into()).await?;
    print_value(&output, matrix.output)?;
    Ok(())
}

fn dispatch_enter(matrix: &Matrix, context: ContextArgs) -> Result<Value> {
    let executable = env::var("MATRIX_ENTER_BIN").unwrap_or_else(|_| "matrix-enter".to_string());
    let mut command = ProcessCommand::new(&executable);

    if let Some(construct) = matrix.construct.as_deref() {
        command.arg("--construct").arg(construct);
    }
    command.arg("--api-prefix").arg(&matrix.api_prefix);
    match matrix.output {
        OutputFormat::Json => {
            command.arg("-o").arg("json");
        }
        OutputFormat::Yaml => {
            command.arg("-o").arg("yaml");
        }
        OutputFormat::Table => {
            command.arg("-o").arg("table");
        }
        OutputFormat::Csv => {
            command.arg("-o").arg("csv");
        }
        OutputFormat::Human => {}
    }
    append_context_args(&mut command, &context);

    let status = command.status().with_context(|| {
        format!(
            "failed to start {executable:?}; install matrix-enter or set MATRIX_ENTER_BIN to its path"
        )
    })?;
    if !status.success() {
        bail!("{executable} exited with {status}");
    }
    Ok(json!({"status":"left", "binary": executable}))
}

fn append_context_args(command: &mut ProcessCommand, context: &ContextArgs) {
    if let Some(zone) = context.zone.as_deref() {
        command.arg("--zone").arg(zone);
    }
    if let Some(repo) = context.repo.as_deref() {
        command.arg("--repo").arg(repo);
    }
    if let Some(component) = context.component.as_deref() {
        command.arg("--component").arg(component);
    }
    if let Some(tag) = context.tag.as_deref() {
        command.arg("--tag").arg(tag);
    }
    if let Some(version) = context.version.as_deref() {
        command.arg("--target-version").arg(version);
    }
    if let Some(sha) = context.sha.as_deref() {
        command.arg("--sha").arg(sha);
    }
    if let Some(reference) = context.r#ref.as_deref() {
        command.arg("--ref").arg(reference);
    }
}

impl Matrix {
    fn load(
        construct_override: Option<String>,
        prefix_override: Option<String>,
        output: OutputFormat,
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
            output,
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

impl MatrixContext {
    fn detect(args: ContextArgs) -> Self {
        let repo_was_overridden = args.repo.is_some();
        let use_git_source_context = !repo_was_overridden;
        Self {
            zone: args.zone.or_else(|| env::var("MATRIX_ZONE").ok()),
            repo: args.repo.or_else(current_repo),
            component: args.component,
            tag: args
                .tag
                .or_else(|| use_git_source_context.then(current_exact_tag).flatten()),
            version: args.version,
            sha: args
                .sha
                .or_else(|| use_git_source_context.then(current_sha).flatten()),
            reference: args
                .r#ref
                .or_else(|| use_git_source_context.then(current_branch).flatten()),
        }
    }
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

async fn query(matrix: &Matrix, args: QueryArgs) -> Result<Value> {
    let facts = fetch_facts(matrix, args.max_facts).await?;
    let context = MatrixContext::detect(args.context);
    let db = build_facts_db(&facts, &context)?;
    execute_readonly_sql(&db, &args.sql)
}

#[cfg(feature = "interactive")]
async fn enter(matrix: &Matrix, context: ContextArgs) -> Result<Value> {
    let mut repl = ReplSession::new(matrix, MatrixContext::detect(context)).await?;
    repl.run().await?;
    Ok(json!({"status":"left"}))
}

#[cfg(feature = "interactive")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Table,
    Json,
    Yaml,
    Csv,
}

#[cfg(feature = "interactive")]
struct ReplSession<'a> {
    matrix: &'a Matrix,
    context: MatrixContext,
    facts: Vec<Value>,
    choices: Vec<ContextChoice>,
    db: Connection,
    max_facts: usize,
    fact_count: usize,
    output_mode: OutputMode,
    expanded: bool,
    timing: bool,
    last_refresh: SystemTime,
}

#[cfg(feature = "interactive")]
impl<'a> ReplSession<'a> {
    async fn new(matrix: &'a Matrix, context: MatrixContext) -> Result<Self> {
        let max_facts = 1000;
        let facts = fetch_facts(matrix, max_facts).await?;
        let fact_count = facts.len();
        Ok(Self {
            matrix,
            db: build_facts_db(&facts, &context)?,
            context,
            facts,
            choices: Vec::new(),
            max_facts,
            fact_count,
            output_mode: match matrix.output {
                OutputFormat::Json => OutputMode::Json,
                OutputFormat::Yaml => OutputMode::Yaml,
                OutputFormat::Csv => OutputMode::Csv,
                OutputFormat::Human | OutputFormat::Table => OutputMode::Table,
            },
            expanded: false,
            timing: false,
            last_refresh: SystemTime::now(),
        })
    }

    async fn refresh(&mut self) -> Result<()> {
        let facts = fetch_facts(self.matrix, self.max_facts).await?;
        self.fact_count = facts.len();
        self.facts = facts;
        self.db = build_facts_db(&self.facts, &self.context)?;
        self.last_refresh = SystemTime::now();
        Ok(())
    }

    fn rebuild_context(&mut self) -> Result<()> {
        self.db = build_facts_db(&self.facts, &self.context)?;
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
                self.context = MatrixContext::detect(ContextArgs::default());
                self.rebuild_context()?;
                eprintln!("Session context reset to auto-detected values.");
            }
            "help" | "?" => print_repl_help(),
            "context" | "ctx" => self.handle_context_command(parts.collect::<Vec<_>>())?,
            "zone" | "repo" | "component" | "version" | "tag" | "sha" | "ref" => {
                self.set_context_field(name, &parts.collect::<Vec<_>>().join(" "))?;
            }
            "components" => self.list_components()?,
            "versions" => self.list_versions(parts.collect::<Vec<_>>().join(" "))?,
            "tags" => self.list_tags()?,
            "use" => self.use_context_choice(parts.collect::<Vec<_>>())?,
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
                Some("yaml") | Some("yml") => {
                    self.output_mode = OutputMode::Yaml;
                    eprintln!("Output mode: yaml");
                }
                Some("csv") => {
                    self.output_mode = OutputMode::Csv;
                    eprintln!("Output mode: csv");
                }
                Some(other) => {
                    eprintln!("Unknown mode {other:?}; expected table, json, yaml, or csv.")
                }
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
                    print_value(&value, self.matrix.output)?;
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
            "context": self.context_json()?,
            "facts": self.fact_count,
            "maxFacts": self.max_facts,
            "mode": format!("{:?}", self.output_mode).to_ascii_lowercase(),
            "expanded": self.expanded,
            "timing": self.timing,
            "refreshed": refreshed,
        });
        print_value(&value, self.matrix.output)
    }

    fn context_json(&self) -> Result<Value> {
        let result = execute_readonly_sql(
            &self.db,
            "select zone, repo, component, version, tag, sha, ref from context",
        )?;
        Ok(result["rows"]
            .as_array()
            .and_then(|rows| rows.first())
            .cloned()
            .unwrap_or_else(|| json!({})))
    }

    fn print_context(&self) -> Result<()> {
        print_value(&self.context_json()?, self.matrix.output)
    }

    fn handle_context_command(&mut self, args: Vec<&str>) -> Result<()> {
        match args.as_slice() {
            [] | ["show"] => self.print_context(),
            ["set", field, rest @ ..] => self.set_context_field(field, &rest.join(" ")),
            ["auto"] => {
                self.context = MatrixContext::detect(ContextArgs::default());
                self.rebuild_context()?;
                self.print_context()
            }
            ["clear"] | ["clear", "all"] => {
                self.context = MatrixContext::default();
                self.rebuild_context()?;
                self.print_context()
            }
            ["clear", field] => {
                self.clear_context_field(field)?;
                self.print_context()
            }
            [field, rest @ ..] => self.set_context_field(field, &rest.join(" ")),
        }
    }

    fn set_context_field(&mut self, field: &str, value: &str) -> Result<()> {
        let value = value.trim();
        if value.is_empty() {
            bail!("usage: .{field} <value>");
        }
        match field {
            "zone" => self.context.zone = Some(value.to_string()),
            "repo" => {
                self.context.repo = Some(value.to_string());
                self.context.tag = None;
                self.context.sha = None;
                self.context.reference = None;
            }
            "component" => self.context.component = Some(value.to_string()),
            "version" => self.context.version = Some(value.to_string()),
            "tag" => self.context.tag = Some(value.to_string()),
            "sha" => self.context.sha = Some(value.to_string()),
            "ref" => self.context.reference = Some(value.to_string()),
            _ => bail!("unknown context field {field:?}"),
        }
        self.rebuild_context()?;
        self.print_context()
    }

    fn clear_context_field(&mut self, field: &str) -> Result<()> {
        match field {
            "zone" => self.context.zone = None,
            "repo" => self.context.repo = None,
            "component" => self.context.component = None,
            "version" => self.context.version = None,
            "tag" => self.context.tag = None,
            "sha" => self.context.sha = None,
            "ref" => self.context.reference = None,
            _ => bail!("unknown context field {field:?}"),
        }
        self.rebuild_context()
    }

    fn list_components(&mut self) -> Result<()> {
        let sql = format!(
            "select row_number() over (order by max(observed_at) desc, component asc) as pick,
                    component, repo, zone, count(*) as facts, max(observed_at) as last_observed_at
             from components
             where component is not null {}
             group by component, repo, zone
             order by last_observed_at desc, component asc
             limit 50",
            self.context_filter_sql("components", &["repo", "zone"])
        );
        self.run_choice_sql("component", "component", &sql)
    }

    fn list_versions(&mut self, component_override: String) -> Result<()> {
        let component_filter = if component_override.trim().is_empty() {
            String::new()
        } else {
            let component = sql_literal(&component_key(component_override.trim()));
            let exact = sql_literal(component_override.trim());
            format!(" and (component = {component} or component = {exact})")
        };
        let sql = format!(
            "select row_number() over (order by max(observed_at) desc, version desc) as pick,
                    version, component, repo, zone, status, count(*) as facts, max(observed_at) as last_observed_at
             from components
             where version is not null {} {}
             group by version, component, repo, zone, status
             order by last_observed_at desc, version desc
             limit 50",
            self.context_filter_sql("components", &["repo", "zone", "component"]),
            component_filter
        );
        self.run_choice_sql("version", "version", &sql)
    }

    fn list_tags(&mut self) -> Result<()> {
        let sql = format!(
            "select row_number() over (order by max(observed_at) desc, tag desc) as pick,
                    tag, component, repo, zone, status, count(*) as facts, max(observed_at) as last_observed_at
             from facts
             where tag is not null {}
             group by tag, component, repo, zone, status
             order by last_observed_at desc, tag desc
             limit 50",
            self.context_filter_sql("facts", &["repo", "zone", "component"])
        );
        self.run_choice_sql("tag", "tag", &sql)
    }

    fn run_choice_sql(&mut self, field: &'static str, value_column: &str, sql: &str) -> Result<()> {
        let result = execute_readonly_sql(&self.db, sql)?;
        self.choices = result["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                row.get(value_column)
                    .and_then(Value::as_str)
                    .map(|value| ContextChoice {
                        field,
                        value: value.to_string(),
                    })
            })
            .collect();
        print_query_result(&result, self.output_mode, self.expanded)?;
        if !self.choices.is_empty() {
            eprintln!("Use `.use <pick>` to focus one of these {} values.", field);
        }
        Ok(())
    }

    fn use_context_choice(&mut self, args: Vec<&str>) -> Result<()> {
        match args.as_slice() {
            [pick] if pick.parse::<usize>().is_ok() => {
                let index = pick.parse::<usize>()?;
                let choice = self
                    .choices
                    .get(index.saturating_sub(1))
                    .cloned()
                    .ok_or_else(|| anyhow!("pick {index} is not available; run `.versions`, `.tags`, or `.components` first"))?;
                self.set_context_field(choice.field, &choice.value)
            }
            [field, rest @ ..] if !rest.is_empty() => {
                self.set_context_field(field, &rest.join(" "))
            }
            _ => bail!("usage: .use <pick> or .use <field> <value>"),
        }
    }

    fn context_filter_sql(&self, table: &str, fields: &[&str]) -> String {
        let mut filters = Vec::new();
        if fields.contains(&"zone")
            && let Some(zone) = self.effective_zone().or_else(|| self.context.zone.clone())
        {
            filters.push(format!("{table}.zone = {}", sql_literal(&zone)));
        }
        if fields.contains(&"repo")
            && let Some(repo) = self.context.repo.as_deref()
        {
            filters.push(format!("{table}.repo = {}", sql_literal(repo)));
        }
        if fields.contains(&"component")
            && let Some(component) = self.context.component.as_deref()
        {
            let key = sql_literal(&component_key(component));
            let exact = sql_literal(component);
            filters.push(format!(
                "({table}.component = {key} or {table}.component = {exact})"
            ));
        }
        if filters.is_empty() {
            String::new()
        } else {
            format!(" and {}", filters.join(" and "))
        }
    }

    fn effective_zone(&self) -> Option<String> {
        self.context_json().ok().and_then(|value| {
            value
                .get("zone")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
    }
}

#[cfg(feature = "interactive")]
#[derive(Clone, Debug)]
struct ContextChoice {
    field: &'static str,
    value: String,
}

fn build_facts_db(facts: &[Value], context: &MatrixContext) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(
        "create table facts (
          id text, zone text, kind text, status text,
          type text, component text, version text, repo text,
          source_repository text, source_repo text, source_sha text, source_ref text,
          subject_type text, subject_name text, channel text,
          tag text, observed_at text, accepted_at text,
          requires text, provides text, json text not null
        );",
    )?;
    let mut zones = Vec::new();
    for record in facts {
        let fact = record
            .get("fact")
            .filter(|value| value.is_object())
            .unwrap_or(record);
        let zone = text_at(fact, &["track"])
            .or_else(|| text_at(record, &["track"]))
            .or_else(|| text_at(fact, &["zone"]))
            .or_else(|| text_at(record, &["zone"]));
        if let Some(zone) = zone.clone()
            && is_sql_identifier(&zone)
            && !zones.contains(&zone)
        {
            zones.push(zone);
        }
        let subject_type =
            text_at(fact, &["subjectType"]).or_else(|| text_at(fact, &["subject", "type"]));
        let subject_name =
            text_at(fact, &["subjectName"]).or_else(|| text_at(fact, &["subject", "name"]));
        let component = subject_name.as_deref().map(component_key);
        let subject_repo =
            text_at(fact, &["subjectRepo"]).or_else(|| text_at(fact, &["subject", "repo"]));
        let source_repo = text_at(fact, &["sourceRepository"])
            .or_else(|| text_at(record, &["sourceRepository"]))
            .or_else(|| text_at(fact, &["source", "repo"]))
            .or_else(|| text_at(record, &["source", "repo"]))
            .or_else(|| text_at(record, &["source", "repository"]));
        let source_sha = text_at(fact, &["sourceSha"])
            .or_else(|| text_at(record, &["sourceSha"]))
            .or_else(|| text_at(fact, &["source", "sha"]))
            .or_else(|| text_at(record, &["source", "sha"]));
        let source_ref = text_at(fact, &["sourceRef"])
            .or_else(|| text_at(record, &["sourceRef"]))
            .or_else(|| text_at(fact, &["source", "ref"]))
            .or_else(|| text_at(record, &["source", "ref"]));
        let tag = text_at(fact, &["tag"]).or_else(|| source_ref.clone());
        db.execute(
            "insert into facts values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                text_at(fact, &["id"]).or_else(|| text_at(record, &["id"])),
                zone,
                text_at(fact, &["kind"]).or_else(|| text_at(record, &["kind"])),
                text_at(fact, &["status"]),
                subject_type.clone(),
                component,
                text_at(fact, &["subjectVersion"]).or_else(|| text_at(fact, &["subject", "version"])),
                subject_repo.clone().or_else(|| source_repo.clone()),
                source_repo.clone(),
                source_repo,
                source_sha,
                source_ref,
                subject_type,
                subject_name,
                text_at(fact, &["channel"]),
                tag,
                text_at(fact, &["observedAt"]).or_else(|| text_at(record, &["observedAt"])),
                text_at(record, &["acceptedAt"]).or_else(|| text_at(fact, &["acceptedAt"])),
                json_array_text(fact.get("requires"))?,
                json_array_text(fact.get("provides"))?,
                serde_json::to_string(record)?,
            ],
        )?;
    }
    create_matrix_views(&db, context, &zones)?;
    Ok(db)
}

fn create_matrix_views(db: &Connection, context: &MatrixContext, zones: &[String]) -> Result<()> {
    db.execute_batch(
        "create view zones as
          select zone, count(*) as facts,
                 sum(case when status in ('compatible', 'passed', 'observed', 'candidate') then 1 else 0 end) as valid,
                 sum(case when status in ('incompatible', 'failed') then 1 else 0 end) as invalid
          from facts
          where zone is not null
          group by zone;
        create view subjects as
          select type, component, repo, count(*) as facts,
                 max(observed_at) as last_observed_at
          from facts
          where component is not null
          group by type, component, repo;
        create view components as
          select zone, type, component, repo, version, status,
                 case
                   when status in ('compatible', 'passed', 'observed', 'candidate', 'valid', 'ready') then 'valid'
                   when status in ('incompatible', 'failed', 'invalid', 'blocked') then 'invalid'
                   else coalesce(status, 'unknown')
                 end as status_class,
                 observed_at, accepted_at, id
          from facts
          where component is not null;
        create view valid_facts as
          select * from facts
          where status in ('compatible', 'passed', 'observed', 'candidate', 'valid', 'ready');
        create view invalid_facts as
          select * from facts
          where status in ('incompatible', 'failed', 'invalid', 'blocked');
        create view requirements as
          select f.id as fact_id, f.zone, f.type, f.component, f.repo, f.version, f.status,
                 json_extract(item.value, '$.capability') as capability,
                 json_extract(item.value, '$.version') as capability_version,
                 item.value as requirement
          from facts f, json_each(coalesce(f.requires, '[]')) item;
        create view capabilities as
          select f.id as fact_id, f.zone, f.type, f.component, f.repo, f.version, f.status,
                 json_extract(item.value, '$.capability') as capability,
                 json_extract(item.value, '$.version') as capability_version,
                 item.value as provides
          from facts f, json_each(coalesce(f.provides, '[]')) item;",
    )?;

    let active_where = active_context_where(context);
    let active_zone = context
        .zone
        .clone()
        .or_else(|| infer_context_zone(db, &active_where).ok().flatten());
    let zone_where = active_zone
        .as_deref()
        .map(|zone| format!("zone = {}", sql_literal(zone)))
        .unwrap_or_else(|| "0".to_string());
    db.execute_batch(&format!(
        "create view context as select
           {} as zone,
           {} as repo,
           {} as component,
           {} as version,
           {} as tag,
           {} as sha,
           {} as ref;
         create view active as select * from facts where {active_where};
         create view zone as select * from facts where {zone_where};",
        sql_literal_opt(active_zone.as_deref()),
        sql_literal_opt(context.repo.as_deref()),
        sql_literal_opt(context.component.as_deref()),
        sql_literal_opt(context.version.as_deref()),
        sql_literal_opt(context.tag.as_deref()),
        sql_literal_opt(context.sha.as_deref()),
        sql_literal_opt(context.reference.as_deref()),
    ))?;

    for zone in zones {
        if matches!(
            zone.as_str(),
            "zone"
                | "active"
                | "facts"
                | "context"
                | "subjects"
                | "components"
                | "valid_facts"
                | "invalid_facts"
                | "requirements"
                | "capabilities"
                | "zones"
        ) {
            continue;
        }
        db.execute_batch(&format!(
            "create view {} as select * from facts where zone = {};",
            quote_identifier(zone),
            sql_literal(zone)
        ))?;
    }

    Ok(())
}

fn active_context_where(context: &MatrixContext) -> String {
    let mut filters = Vec::new();
    if let Some(repo) = context.repo.as_deref() {
        let repo = sql_literal(repo);
        filters.push(format!(
            "(repo = {repo} or source_repo = {repo} or source_repository = {repo})"
        ));
    }
    if let Some(component) = context.component.as_deref() {
        let exact = sql_literal(component);
        let key = sql_literal(&component_key(component));
        filters.push(format!("(component = {key} or subject_name = {exact})"));
    }
    if let Some(version) = context.version.as_deref() {
        filters.push(format!("version = {}", sql_literal(version)));
    }
    if let Some(sha) = context.sha.as_deref() {
        filters.push(format!("source_sha = {}", sql_literal(sha)));
    }
    if let Some(reference) = context.reference.as_deref() {
        filters.push(format!("source_ref = {}", sql_literal(reference)));
    }
    if let Some(tag) = context.tag.as_deref() {
        let tag = sql_literal(tag);
        filters.push(format!(
            "(tag = {tag} or version = {tag} or source_ref = {tag})"
        ));
    }
    if filters.is_empty() {
        "1".to_string()
    } else {
        filters.join(" and ")
    }
}

fn infer_context_zone(db: &Connection, active_where: &str) -> Result<Option<String>> {
    let sql = format!(
        "select zone from facts where {active_where} and zone is not null order by observed_at desc, id asc limit 1"
    );
    match db.query_row(&sql, [], |row| row.get::<_, String>(0)) {
        Ok(zone) => Ok(Some(zone)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn text_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(ToString::to_string)
}

fn component_key(value: &str) -> String {
    value
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(value)
        .trim_start_matches('@')
        .to_string()
}

fn json_array_text(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        Some(Value::Array(_)) => Ok(Some(serde_json::to_string(value.unwrap())?)),
        Some(Value::Null) | None => Ok(None),
        Some(other) => Ok(Some(serde_json::to_string(&vec![other.clone()])?)),
    }
}

fn is_sql_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn sql_literal_opt(value: Option<&str>) -> String {
    value.map(sql_literal).unwrap_or_else(|| "null".to_string())
}

fn normalize_matrix_sql(sql: &str) -> String {
    let tokens = tokenize_sql(&sql.replace("==", "="));
    let mut normalized = tokens.clone();
    let comparable = [
        "zone",
        "kind",
        "status",
        "type",
        "component",
        "repo",
        "version",
        "source_repo",
        "source_repository",
        "subject_name",
        "subject_type",
    ];

    let mut index = 0;
    while index < tokens.len() {
        let field = comparable_field_name(&tokens[index]);
        if comparable.contains(&field.as_str())
            && token_boundary_before(&tokens, index)
            && token_boundary_after(&tokens, index)
            && let Some(op_index) = next_non_ws(&tokens, index + 1)
            && matches!(tokens[op_index].as_str(), "=" | "!=" | "<>")
            && let Some(value_index) = next_non_ws(&tokens, op_index + 1)
            && is_bare_sql_value(&tokens[value_index])
        {
            normalized[value_index] = sql_literal(&tokens[value_index]);
            index = value_index + 1;
            continue;
        }
        index += 1;
    }

    normalize_status_class_filters(normalized).join("")
}

fn normalize_status_class_filters(tokens: Vec<String>) -> Vec<String> {
    let mut normalized = tokens.clone();
    let mut index = 0;
    while index < tokens.len() {
        let field = comparable_field_name(&tokens[index]);
        if field == "status"
            && token_boundary_before(&tokens, index)
            && token_boundary_after(&tokens, index)
            && let Some(op_index) = next_non_ws(&tokens, index + 1)
            && matches!(tokens[op_index].as_str(), "=" | "!=" | "<>")
            && let Some(value_index) = next_non_ws(&tokens, op_index + 1)
            && let Some(status_class) = status_class_value(&tokens[value_index])
        {
            let negated = matches!(tokens[op_index].as_str(), "!=" | "<>");
            normalized[index] = format!(
                "{} {} {}",
                tokens[index],
                if negated { "not in" } else { "in" },
                status_class_sql(status_class),
            );
            for token in normalized.iter_mut().take(value_index + 1).skip(index + 1) {
                token.clear();
            }
            index = value_index + 1;
            continue;
        }
        index += 1;
    }
    normalized
}

fn status_class_value(token: &str) -> Option<&'static str> {
    let value = token
        .trim()
        .trim_matches('\'')
        .trim_matches('"')
        .to_ascii_lowercase();
    match value.as_str() {
        "valid" => Some("valid"),
        "invalid" => Some("invalid"),
        _ => None,
    }
}

fn status_class_sql(status_class: &str) -> &'static str {
    match status_class {
        "valid" => "('compatible','passed','observed','candidate','valid','ready')",
        "invalid" => "('incompatible','failed','invalid','blocked')",
        _ => "('')",
    }
}

fn comparable_field_name(token: &str) -> String {
    token
        .rsplit_once('.')
        .map(|(_, field)| field)
        .unwrap_or(token)
        .to_ascii_lowercase()
}

fn tokenize_sql(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars = sql.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current.is_whitespace() {
            let start = index;
            while index < chars.len() && chars[index].is_whitespace() {
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
        } else if current == '\'' || current == '"' {
            let quote = current;
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
        } else if is_sql_word_char(current) {
            let start = index;
            while index < chars.len() && is_sql_word_char(chars[index]) {
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
        } else if index + 1 < chars.len()
            && matches!(
                (current, chars[index + 1]),
                ('!', '=') | ('<', '>') | ('<', '=') | ('>', '=')
            )
        {
            tokens.push(chars[index..index + 2].iter().collect());
            index += 2;
        } else {
            tokens.push(current.to_string());
            index += 1;
        }
    }
    tokens
}

fn is_sql_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '_' | '-' | '@' | '.' | '/' | ':' | '#')
}

fn token_boundary_before(tokens: &[String], index: usize) -> bool {
    index == 0
        || tokens[index - 1].trim().is_empty()
        || tokens[index - 1] == "."
        || matches!(tokens[index - 1].as_str(), "(" | ",")
}

fn token_boundary_after(tokens: &[String], index: usize) -> bool {
    tokens
        .get(index + 1)
        .map(|token| {
            token.trim().is_empty() || matches!(token.as_str(), "=" | "!" | "<" | ">" | "!=" | "<>")
        })
        .unwrap_or(true)
}

fn next_non_ws(tokens: &[String], start: usize) -> Option<usize> {
    (start..tokens.len()).find(|index| !tokens[*index].trim().is_empty())
}

fn is_bare_sql_value(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    !token.starts_with('\'')
        && !token.starts_with('"')
        && token.parse::<f64>().is_err()
        && !matches!(
            lower.as_str(),
            "null"
                | "true"
                | "false"
                | "select"
                | "from"
                | "where"
                | "and"
                | "or"
                | "in"
                | "like"
                | "is"
                | "not"
        )
}

fn execute_readonly_sql(db: &Connection, sql: &str) -> Result<Value> {
    let sql = normalize_matrix_sql(sql);
    let normalized = sql.trim().to_ascii_lowercase();
    if !(normalized.starts_with("select ")
        || normalized.starts_with("with ")
        || normalized.starts_with("explain query plan "))
    {
        bail!("matrix query only allows read-only SELECT/WITH/EXPLAIN statements");
    }
    let mut stmt = db.prepare(&sql)?;
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

#[cfg(feature = "interactive")]
fn print_query_result(value: &Value, mode: OutputMode, expanded: bool) -> Result<()> {
    match mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(value)?),
        OutputMode::Yaml => print_yaml(value)?,
        OutputMode::Csv => print_csv_result(value)?,
        OutputMode::Table => print_table_result(value, expanded)?,
    }
    Ok(())
}

#[cfg(feature = "interactive")]
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
                    table_cell_value(row.get(column).unwrap_or(&Value::Null))
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
                .map(|column| table_cell_value(row.get(column).unwrap_or(&Value::Null)))
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

fn table_cell_value(value: &Value) -> String {
    match value {
        Value::Null => "".to_string(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => {
            if *value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::Array(values) => count_label(values.len(), "item"),
        Value::Object(object) => count_label(object.len(), "field"),
    }
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(feature = "interactive")]
fn print_repl_help() {
    eprintln!(
        "\
Matrix shell commands
  .help, /help              Show this help
  .status, /status          Show session, construct, cache, and output state
  .tables                   List local tables and views
  .schema [table]           Show local SQL schema
  .describe [table]         Show columns for a table or view
  .mode table|json|yaml|csv Change output format
  .x                        Toggle expanded table output
  .timing                   Toggle query timing
  .limit <n>                Set fact fetch limit and refresh cache
  .refresh                  Reload facts from the construct
  .context                  Show active context
  .context <field> <value>  Set zone, repo, component, version, tag, sha, or ref
  .context set <field> ...  Same as `.context <field> <value>`
  .context clear [field]    Clear one context field, or all fields
  .context auto             Reset to detected git repo, tag, ref, and sha
  .zone/.repo/.component    Shortcut setters for common context fields
  .version/.tag/.sha/.ref   Shortcut setters for version and source context
  .components               List components in the current context
  .versions [component]     List versions and remember picks for `.use`
  .tags                     List tags/refs and remember picks for `.use`
  .use <pick>               Focus a picked component, version, or tag
  .zones                    Summarize facts by zone
  .subjects                 Summarize facts by subject
  .trace <subject>          Show recent facts for a subject
  .gate <zone> [level]      Fetch a gate decision from the construct
  .explain <sql>            Run EXPLAIN QUERY PLAN
  red, red-pill, .exit      Exit
  blue, blue-pill           Clear the current session context

SQL
  End SQL statements with `;`.
  Available tables/views: facts, active, zone, zones, subjects, components,
  valid_facts, invalid_facts, capabilities, requirements, and one view per
  SQL-safe zone such as odin. `status = valid` expands to compatible statuses.
"
    );
}

#[cfg(feature = "interactive")]
fn is_repl_command(line: &str) -> bool {
    line.starts_with('.')
        || line.starts_with('/')
        || matches!(
            line,
            "red" | "red-pill" | "blue" | "blue-pill" | "exit" | "quit" | "help"
        )
}

#[cfg(feature = "interactive")]
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

#[cfg(feature = "interactive")]
fn repl_history_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "matrix", "matrix")
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dirs.data_dir().join("repl-history.txt"))
}

#[cfg(feature = "interactive")]
struct MatrixCompleter {
    words: Vec<String>,
}

#[cfg(feature = "interactive")]
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
            ".component",
            ".components",
            ".context",
            ".repo",
            ".subjects",
            ".tables",
            ".tag",
            ".tags",
            ".timing",
            ".trace",
            ".use",
            ".version",
            ".versions",
            ".zone",
            ".zones",
            "/help",
            "/status",
            "blue",
            "by",
            "capabilities",
            "capability",
            "channel",
            "compatible",
            "component",
            "components",
            "context",
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
            "accepted_at",
            "order",
            "provides",
            "red",
            "repo",
            "ref",
            "requirements",
            "requires",
            "select",
            "source_repository",
            "source_repo",
            "source_sha",
            "status",
            "subject_name",
            "subject_type",
            "subjects",
            "table",
            "tag",
            "tags",
            "type",
            "use",
            "version",
            "versions",
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

#[cfg(feature = "interactive")]
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

#[cfg(feature = "interactive")]
struct MatrixHighlighter;

#[cfg(feature = "interactive")]
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

#[cfg(feature = "interactive")]
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

fn print_value(value: &Value, output: OutputFormat) -> Result<()> {
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(value)?),
        OutputFormat::Yaml => print_yaml(value)?,
        OutputFormat::Csv => {
            if is_query_result(value) {
                print_csv_result(value)?;
            } else {
                bail!("csv output is only available for tabular query results");
            }
        }
        OutputFormat::Table => {
            if is_query_result(value) {
                print_plain_table_result(value)?;
            } else {
                print_generic_table(value);
            }
        }
        OutputFormat::Human => {
            if is_query_result(value) {
                print_plain_table_result(value)?;
            } else {
                print_human_value(value);
            }
        }
    }
    Ok(())
}

fn print_yaml(value: &Value) -> Result<()> {
    print!("{}", serde_yaml::to_string(value)?);
    Ok(())
}

fn is_query_result(value: &Value) -> bool {
    value.get("columns").is_some_and(Value::is_array)
        && value.get("rows").is_some_and(Value::is_array)
}

fn print_human_value(value: &Value) {
    match value {
        Value::Null => println!("No data."),
        Value::String(text) => println!("{text}"),
        Value::Bool(value) => println!("{}", if *value { "yes" } else { "no" }),
        Value::Number(value) => println!("{value}"),
        Value::Array(values) => print_human_array(values),
        Value::Object(object) => print_human_object(object),
    }
}

fn print_human_array(values: &[Value]) {
    if values.is_empty() {
        println!("No items.");
        return;
    }
    for value in values {
        match value {
            Value::String(text) => println!("- {text}"),
            Value::Number(number) => println!("- {number}"),
            Value::Bool(value) => println!("- {}", if *value { "yes" } else { "no" }),
            Value::Null => println!("-"),
            other => println!("- {}", human_inline_value(other)),
        }
    }
}

fn print_human_object(object: &serde_json::Map<String, Value>) {
    if let Some(saved) = object.get("saved").and_then(Value::as_str) {
        println!("Saved {saved}");
        return;
    }
    if let Some(accepted) = object.get("accepted").and_then(Value::as_u64) {
        println!(
            "Accepted {accepted} fact{}.",
            if accepted == 1 { "" } else { "s" }
        );
        return;
    }
    if let Some(zones) = object.get("zones").and_then(Value::as_array) {
        println!("Zones");
        if zones.is_empty() {
            println!("  none");
        } else {
            for zone in zones {
                println!("  {}", human_inline_value(zone));
            }
        }
        if let Some(generated_at) = object.get("generatedAt").and_then(Value::as_str) {
            println!("Generated: {generated_at}");
        }
        return;
    }
    if object.contains_key("configPath")
        || object.contains_key("construct")
        || object.contains_key("apiPrefix")
        || object.contains_key("reachable")
    {
        println!("Matrix");
        print_object_field(object, "configPath", "Config");
        print_object_field(object, "construct", "Construct");
        print_object_field(object, "apiPrefix", "API prefix");
        print_object_field(object, "hasToken", "Token");
        print_object_field(object, "reachable", "Reachable");
        return;
    }
    if let (Some(zone), Some(level), Some(gate)) = (
        object.get("zone").and_then(Value::as_str),
        object.get("level").and_then(Value::as_str),
        object.get("gate").and_then(Value::as_object),
    ) {
        println!("Gate: {zone} / {level}");
        print_object_field(gate, "status", "Status");
        print_object_field(gate, "eligible", "Eligible");
        print_object_field(gate, "failedFacts", "Failed facts");
        print_object_field(gate, "totalFacts", "Total facts");
        return;
    }
    for (key, value) in object {
        println!("{}: {}", human_label(key), human_inline_value(value));
    }
}

fn print_object_field(object: &serde_json::Map<String, Value>, key: &str, label: &str) {
    if let Some(value) = object.get(key) {
        println!("{label}: {}", human_inline_value(value));
    }
}

fn human_inline_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => {
            if *value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::Array(values) => count_label(values.len(), "item"),
        Value::Object(_) => value.to_string(),
    }
}

fn count_label(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn human_label(key: &str) -> String {
    let mut label = String::new();
    for (index, character) in key.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            label.push(' ');
        }
        label.push(character);
    }
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => label,
    }
}

fn print_plain_table_result(value: &Value) -> Result<()> {
    let columns = value["columns"].as_array().cloned().unwrap_or_default();
    let rows = value["rows"].as_array().cloned().unwrap_or_default();
    let column_names = columns
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    print!("{}", plain_table_result_text(&column_names, &rows));
    Ok(())
}

fn print_generic_table(value: &Value) {
    print!("{}", generic_table_text(value));
}

fn generic_table_text(value: &Value) -> String {
    match value {
        Value::Object(object) => object_table_text(object),
        Value::Array(values) => array_table_text(values),
        _ => object_table_text(&serde_json::Map::from_iter([(
            "value".to_string(),
            value.clone(),
        )])),
    }
}

fn object_table_text(object: &serde_json::Map<String, Value>) -> String {
    let rows = object
        .iter()
        .map(|(key, value)| json!({"field": human_label(key), "value": table_cell_value(value)}))
        .collect::<Vec<_>>();
    plain_table_result_text(&["field".to_string(), "value".to_string()], &rows)
}

fn array_table_text(values: &[Value]) -> String {
    if values.is_empty() {
        return plain_table_result_text(&["value".to_string()], &[]);
    }

    if values.iter().all(Value::is_object) {
        let mut column_names = Vec::new();
        for value in values {
            if let Some(object) = value.as_object() {
                for key in object.keys() {
                    if !column_names.contains(key) {
                        column_names.push(key.clone());
                    }
                }
            }
        }
        return plain_table_result_text(&column_names, values);
    }

    let rows = values
        .iter()
        .map(|value| json!({"value": table_cell_value(value)}))
        .collect::<Vec<_>>();
    plain_table_result_text(&["value".to_string()], &rows)
}

fn plain_table_result_text(column_names: &[String], rows: &[Value]) -> String {
    let mut text = plain_table_text(column_names, rows);
    text.push_str(&format!("({} rows)\n", rows.len()));
    text
}

fn plain_table_text(column_names: &[String], rows: &[Value]) -> String {
    if column_names.is_empty() {
        return String::new();
    }
    let mut widths = column_names
        .iter()
        .map(|column| column.len())
        .collect::<Vec<_>>();
    let rendered_rows = rows
        .iter()
        .map(|row| {
            column_names
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let cell =
                        truncate_cell(&table_cell_value(row.get(column).unwrap_or(&Value::Null)));
                    widths[index] = widths[index].max(cell.len());
                    cell
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut text = String::new();
    push_plain_separator(&mut text, &widths);
    push_plain_row(&mut text, column_names, &widths);
    push_plain_separator(&mut text, &widths);
    for row in rendered_rows {
        push_plain_row(&mut text, &row, &widths);
    }
    push_plain_separator(&mut text, &widths);
    text
}

fn push_plain_separator(text: &mut String, widths: &[usize]) {
    text.push('+');
    for width in widths {
        text.push_str(&format!("{}+", "-".repeat(*width + 2)));
    }
    text.push('\n');
}

fn push_plain_row(text: &mut String, values: &[String], widths: &[usize]) {
    text.push('|');
    for (value, width) in values.iter().zip(widths) {
        text.push_str(&format!(" {value:<width$} |"));
    }
    text.push('\n');
}

fn truncate_cell(value: &str) -> String {
    const MAX_CELL_WIDTH: usize = 80;
    if value.len() <= MAX_CELL_WIDTH {
        value.replace('\n', "\\n")
    } else {
        format!(
            "{}...",
            value.chars().take(MAX_CELL_WIDTH - 3).collect::<String>()
        )
        .replace('\n', "\\n")
    }
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
    fn accepts_output_before_and_after_top_level_commands() {
        let before = Cli::try_parse_from(["matrix", "-o", "json", "doctor"]).unwrap();
        assert_eq!(before.output, OutputFormat::Json);

        let after = Cli::try_parse_from(["matrix", "doctor", "-o", "json"]).unwrap();
        assert_eq!(after.output, OutputFormat::Json);
    }

    #[test]
    fn accepts_output_after_nested_commands() {
        let cli = Cli::try_parse_from(["matrix", "config", "list", "--out", "yaml"]).unwrap();
        assert_eq!(cli.output, OutputFormat::Yaml);
    }

    #[test]
    fn accepts_output_after_query_sql() {
        let cli = Cli::try_parse_from([
            "matrix",
            "query",
            "select * from facts limit 1",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(cli.output, OutputFormat::Json);
    }

    #[test]
    fn renders_objects_as_field_value_tables() {
        let text = generic_table_text(&json!({
            "track": "odin",
            "eligible": false,
            "blockers": [],
            "latest": {
                "closure": {
                    "edges": []
                },
                "status": "partial"
            },
        }));
        assert!(text.contains("+"));
        assert!(text.contains("| field"));
        assert!(text.contains("| value"));
        assert!(text.contains("| Track"));
        assert!(text.contains("| odin"));
        assert!(text.contains("| Eligible"));
        assert!(text.contains("| no"));
        assert!(text.contains("| Blockers"));
        assert!(text.contains("| 0 items"));
        assert!(text.contains("| Latest"));
        assert!(text.contains("| 2 fields"));
        assert!(!text.contains("{\"closure\""));
    }

    #[test]
    fn renders_object_arrays_as_tables() {
        let text = generic_table_text(&json!([
            {"zone": "odin", "facts": 3},
            {"zone": "agent-admin", "facts": 2}
        ]));
        assert!(text.contains("| zone"));
        assert!(text.contains("| facts"));
        assert!(text.contains("| odin"));
        assert!(text.contains("| agent-admin"));
        assert!(text.contains("(2 rows)"));
    }

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

    #[test]
    fn normalizes_short_matrix_sql() {
        assert_eq!(
            normalize_matrix_sql("select * from zone where type==chaincode and status==failed"),
            "select * from zone where type='chaincode' and status='failed'"
        );
        assert_eq!(
            normalize_matrix_sql("select * from zone where repo==red-wiz/eos and status!=failed"),
            "select * from zone where repo='red-wiz/eos' and status!='failed'"
        );
        assert_eq!(
            normalize_matrix_sql("select * from zone where type==chaincode and status==valid"),
            "select * from zone where type='chaincode' and status in ('compatible','passed','observed','candidate','valid','ready')"
        );
    }

    #[test]
    fn derives_short_component_keys() {
        assert_eq!(component_key("@red-wiz/eos"), "eos");
        assert_eq!(component_key("did_vdr_go"), "did_vdr_go");
    }

    #[test]
    fn repo_override_does_not_inherit_git_source_context() {
        let context = MatrixContext::detect(ContextArgs {
            repo: Some("red-wiz/eos".to_string()),
            ..ContextArgs::default()
        });
        assert_eq!(context.repo.as_deref(), Some("red-wiz/eos"));
        assert!(context.sha.is_none());
        assert!(context.reference.is_none());
        assert!(context.tag.is_none());
    }

    #[test]
    fn forwards_enter_context_to_shell_binary_args() {
        let mut command = ProcessCommand::new("matrix-enter");
        append_context_args(
            &mut command,
            &ContextArgs {
                repo: Some("red-wiz/putto".to_string()),
                version: Some("v0.6.3".to_string()),
                ..ContextArgs::default()
            },
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec!["--repo", "red-wiz/putto", "--target-version", "v0.6.3"]
        );
    }

    #[test]
    fn creates_contextual_zone_view() {
        let facts = vec![
            json!({
                "id": "putto",
                "track": "odin",
                "status": "candidate",
                "source": {"repo": "red-wiz/putto"},
                "subject": {"type": "npm", "name": "@red-wiz/oracle-vdr", "version": "0.6.12", "repo": "red-wiz/putto"}
            }),
            json!({
                "id": "did",
                "track": "odin",
                "status": "observed",
                "subject": {"type": "chaincode", "name": "did_vdr_go", "version": "0.4.8", "repo": "red-wiz/hebe"}
            }),
            json!({
                "id": "other",
                "track": "sdk",
                "status": "observed",
                "subject": {"type": "chaincode", "name": "other", "version": "1.0.0", "repo": "example/other"}
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                repo: Some("red-wiz/putto".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let result = execute_readonly_sql(
            &db,
            "select component, version from zone where type==chaincode order by component",
        )
        .unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["component"], "did_vdr_go");
    }

    #[test]
    fn filters_active_context_by_version_and_component_key() {
        let facts = vec![
            json!({
                "id": "eos-1",
                "track": "odin",
                "status": "candidate",
                "subject": {"type": "npm", "name": "@red-wiz/eos", "version": "0.19.1", "repo": "red-wiz/eos"}
            }),
            json!({
                "id": "eos-2",
                "track": "odin",
                "status": "candidate",
                "subject": {"type": "npm", "name": "@red-wiz/eos", "version": "0.19.2", "repo": "red-wiz/eos"}
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                repo: Some("red-wiz/eos".to_string()),
                component: Some("eos".to_string()),
                version: Some("0.19.2".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let result =
            execute_readonly_sql(&db, "select id, component, version from active").unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["id"], "eos-2");
        assert_eq!(result["rows"][0]["component"], "eos");
    }

    #[test]
    fn flattens_construct_wrapped_facts() {
        let facts = vec![
            json!({
                "acceptedAt": "2026-06-17T21:49:24.064Z",
                "id": "chaincode.csr_vdr_go.0.2.9.6d562df953ec",
                "kind": "CompatibilityFact",
                "source": {"repository": "red-wiz/aglaea"},
                "track": "odin",
                "fact": {
                    "id": "chaincode.csr_vdr_go.0.2.9.6d562df953ec",
                    "kind": "CompatibilityFact",
                    "observedAt": "2026-06-17T21:49:21.392Z",
                    "source": {
                        "ref": "refs/tags/v0.2.9",
                        "repo": "red-wiz/aglaea",
                        "sha": "6d562df953eca829f918a6ea956482f761dccba8"
                    },
                    "status": "candidate",
                    "subject": {
                        "name": "csr_vdr_go",
                        "repo": "red-wiz/aglaea",
                        "type": "chaincode",
                        "version": "0.2.9"
                    },
                    "track": "odin"
                }
            }),
            json!({
                "acceptedAt": "2026-06-17T21:49:24.133Z",
                "id": "validation.odin.csr_vdr_go.0.2.9",
                "kind": "ValidationFact",
                "track": "odin",
                "fact": {
                    "id": "validation.odin.csr_vdr_go.0.2.9",
                    "kind": "ValidationFact",
                    "observedAt": "2026-06-17T21:49:21.392Z",
                    "source": {
                        "repo": "red-wiz/aglaea",
                        "sha": "6d562df953eca829f918a6ea956482f761dccba8"
                    },
                    "status": "not-run",
                    "track": "odin"
                }
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                zone: Some("odin".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let result = execute_readonly_sql(
            &db,
            "select component, version, repo, source_sha, status, observed_at, accepted_at from zone where type==chaincode and status==valid",
        )
        .unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["component"], "csr_vdr_go");
        assert_eq!(result["rows"][0]["version"], "0.2.9");
        assert_eq!(result["rows"][0]["repo"], "red-wiz/aglaea");
        assert_eq!(
            result["rows"][0]["source_sha"],
            "6d562df953eca829f918a6ea956482f761dccba8"
        );
        assert_eq!(result["rows"][0]["status"], "candidate");
        assert_eq!(result["rows"][0]["observed_at"], "2026-06-17T21:49:21.392Z");
        assert_eq!(result["rows"][0]["accepted_at"], "2026-06-17T21:49:24.064Z");
    }

    #[test]
    fn supports_nested_capability_queries() {
        let facts = vec![
            json!({
                "id": "athena",
                "track": "odin",
                "status": "passed",
                "subject": {"type": "npm", "name": "@red-wiz/athena", "version": "1.2.3", "repo": "red-wiz/athena"},
                "provides": [{"capability": "native-askar", "version": "1.2.3"}]
            }),
            json!({
                "id": "eos",
                "track": "odin",
                "status": "candidate",
                "subject": {"type": "service", "name": "eos", "version": "2.0.0", "repo": "red-wiz/eos"},
                "requires": [{"capability": "native-askar", "version": "1.2.3"}]
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                zone: Some("odin".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let result = execute_readonly_sql(
            &db,
            "select id, component from odin
             where repo==red-wiz/eos
               and exists (
                 select 1 from requirements r
                 where r.fact_id = odin.id
                   and r.capability in (
                     select p.capability from capabilities p
                     where p.repo==red-wiz/athena and p.status==passed
                   )
               )",
        )
        .unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["component"], "eos");
    }
}
