use std::{
    env, fs,
    io::{self, IsTerminal, Read},
    path::PathBuf,
    process::{Command as ProcessCommand, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use directories::ProjectDirs;
use reqwest::{Method, header::CONTENT_TYPE};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod ingest;

const MATRIX_REPOSITORY: &str = "adrianmross/matrix";
const MATRIX_HOMEBREW_FORMULA: &str = "adrianmross/tap/matrix";
const UPDATE_CHECK_TTL: Duration = Duration::from_secs(60 * 60 * 24);

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
    Get(FactGetArgs),
    History(HistoryArgs),
    Supersedes(HistoryArgs),
    Upload(UploadArgs),
    Publish(UploadArgs),
    Ingest(IngestArgs),
    Deref(FactQueryArgs),
    Members(FactQueryArgs),
    Components(ListQueryArgs),
    Versions(VersionQueryArgs),
    Tags(ListQueryArgs),
    Upstream(ListQueryArgs),
    Downstream(ListQueryArgs),
    Compatible(ListQueryArgs),
    Compare(CompareArgs),
    Why(CompareArgs),
    Query(QueryArgs),
    Enter(ContextArgs),
    Update(UpdateCommand),
    Doctor,
    RedPill,
    BluePill,
}

#[derive(Args)]
struct UpdateCommand {
    #[arg(long)]
    check: bool,
    #[arg(long)]
    force: bool,
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

#[derive(Args)]
struct HistoryArgs {
    fact_id: String,
    #[arg(long, default_value_t = 25)]
    limit: usize,
    #[command(flatten)]
    selector: RevisionSelectorArgs,
}

#[derive(Args)]
struct FactGetArgs {
    fact_id: String,
    #[arg(long)]
    history: bool,
    #[arg(long, default_value_t = 25)]
    limit: usize,
    #[command(flatten)]
    selector: RevisionSelectorArgs,
}

#[derive(Args, Clone, Default)]
struct RevisionSelectorArgs {
    #[arg(long)]
    revision: Option<i64>,
    #[arg(long)]
    event: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    relative: Option<i64>,
    #[arg(long = "as-of")]
    as_of: Option<String>,
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

#[derive(Args)]
struct FactQueryArgs {
    fact_id: String,
    #[arg(long, default_value_t = 1000)]
    max_facts: usize,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct ListQueryArgs {
    #[arg(long, default_value_t = 1000)]
    max_facts: usize,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long)]
    all: bool,
    #[arg(long = "type")]
    type_filter: Option<String>,
    #[arg(long)]
    include_applications: bool,
    #[arg(long)]
    include_dependencies: bool,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct VersionQueryArgs {
    #[arg(value_name = "COMPONENT")]
    component_filter: Option<String>,
    #[arg(long, default_value_t = 1000)]
    max_facts: usize,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long)]
    all: bool,
    #[arg(long = "type")]
    type_filter: Option<String>,
    #[arg(long)]
    include_applications: bool,
    #[arg(long)]
    include_dependencies: bool,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct CompareArgs {
    target: String,
    #[arg(long)]
    target_version: Option<String>,
    #[arg(long, default_value_t = 1000)]
    max_facts: usize,
    #[arg(long, default_value_t = 50)]
    limit: usize,
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
    #[arg(long = "junit-file")]
    junit_files: Vec<PathBuf>,
    #[arg(long = "junit-glob")]
    junit_globs: Vec<String>,
    #[arg(long)]
    zone: Option<String>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    component: Option<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    sha: Option<String>,
    #[arg(long)]
    r#ref: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    construct: Option<String>,
    api_prefix: Option<String>,
    token: Option<String>,
    sql_init: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sql_packs: Vec<String>,
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
    maybe_print_update_notice(&matrix, &cli.command).await;
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
        Commands::Get(args) => get_fact(&matrix, args).await?,
        Commands::History(args) | Commands::Supersedes(args) => history(&matrix, args).await?,
        Commands::Upload(args) | Commands::Publish(args) => upload(&matrix, args).await?,
        Commands::Ingest(args) => ingest(&matrix, args).await?,
        Commands::Deref(args) => fact_view_query(&matrix, args, FactView::Deref).await?,
        Commands::Members(args) => fact_view_query(&matrix, args, FactView::Members).await?,
        Commands::Components(args) => list_components_query(&matrix, args).await?,
        Commands::Versions(args) => list_versions_query(&matrix, args).await?,
        Commands::Tags(args) => list_tags_query(&matrix, args).await?,
        Commands::Upstream(args) => {
            context_view_query(&matrix, args, ContextView::Upstream).await?
        }
        Commands::Downstream(args) => {
            context_view_query(&matrix, args, ContextView::Downstream).await?
        }
        Commands::Compatible(args) => {
            context_view_query(&matrix, args, ContextView::Compatible).await?
        }
        Commands::Compare(args) | Commands::Why(args) => compare_query(&matrix, args).await?,
        Commands::Query(args) => query(&matrix, args).await?,
        Commands::Enter(context) => dispatch_enter(&matrix, context)?,
        Commands::Update(command) => update_command(&matrix, command).await?,
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

    fn sql_init(&self) -> Result<Option<String>> {
        let mut paths = Vec::new();
        if let Some(path) = env::var("MATRIX_SQL_INIT")
            .ok()
            .or_else(|| self.config.sql_init.clone())
        {
            paths.push(path);
        }
        if let Ok(pack_paths) = env::var("MATRIX_SQL_PACKS") {
            paths.extend(parse_sql_pack_list(&pack_paths));
        } else {
            paths.extend(self.config.sql_packs.clone());
        }
        if paths.is_empty() {
            return Ok(None);
        }

        let mut sql = String::new();
        for path in paths {
            let pack = fs::read_to_string(&path)
                .with_context(|| format!("failed to read Matrix SQL pack {path}"))?;
            sql.push_str(&format!("\n-- Matrix SQL pack: {path}\n"));
            sql.push_str(&pack);
            sql.push('\n');
        }
        Ok(Some(sql))
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
            "sqlInit": matrix.config.sql_init,
            "sqlPacks": matrix.config.sql_packs,
        })),
        ConfigSubcommand::Get { key } => match key.as_str() {
            "construct" => Ok(json!({"construct": matrix.config.construct})),
            "api-prefix" | "apiPrefix" => Ok(json!({"apiPrefix": matrix.config.api_prefix})),
            "token" => Ok(json!({"hasToken": matrix.config.token.is_some()})),
            "sql-init" | "sqlInit" => Ok(json!({"sqlInit": matrix.config.sql_init})),
            "sql-pack" | "sql-packs" | "sqlPack" | "sqlPacks" => {
                Ok(json!({"sqlPacks": matrix.config.sql_packs}))
            }
            _ => bail!(
                "unknown config key {key:?}; expected construct, api-prefix, token, sql-init, sql-pack, or sql-packs"
            ),
        },
        ConfigSubcommand::Set { key, value } => {
            match key.as_str() {
                "construct" => matrix.config.construct = Some(value),
                "api-prefix" | "apiPrefix" => matrix.config.api_prefix = Some(value),
                "token" => matrix.config.token = Some(value),
                "sql-init" | "sqlInit" => matrix.config.sql_init = Some(value),
                "sql-pack" | "sqlPack" => matrix.config.sql_packs = vec![value],
                "sql-packs" | "sqlPacks" => matrix.config.sql_packs = parse_sql_pack_list(&value),
                _ => bail!(
                    "unknown config key {key:?}; expected construct, api-prefix, token, sql-init, sql-pack, or sql-packs"
                ),
            }
            matrix.save()?;
            Ok(json!({"saved": matrix.config_path}))
        }
    }
}

async fn update_command(matrix: &Matrix, command: UpdateCommand) -> Result<Value> {
    if is_homebrew_install() && command.check {
        let current =
            homebrew_installed_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        if let Some(latest) = homebrew_outdated_version() {
            return Ok(json!({
                "current": current,
                "latest": latest,
                "updateAvailable": true,
                "command": "matrix update"
            }));
        }
        return Ok(json!({
            "current": current,
            "latest": current,
            "updateAvailable": false
        }));
    }

    if is_homebrew_install() && !command.check {
        run_status_command("brew", &["update"])?;
        run_status_command("brew", &["upgrade", MATRIX_HOMEBREW_FORMULA])?;
        clear_update_notice_cache();
        return Ok(Value::String(
            "Updated through Homebrew. Run `matrix --version` to confirm the installed version."
                .to_string(),
        ));
    }

    let latest = latest_matrix_version(&matrix.client).await?;
    let current = env!("CARGO_PKG_VERSION");
    let update_available = version_is_newer(current, &latest);

    if command.check {
        if update_available {
            return Ok(json!({
                "current": current,
                "latest": latest,
                "updateAvailable": true,
                "command": "matrix update"
            }));
        }
        return Ok(json!({
            "current": current,
            "latest": latest,
            "updateAvailable": false
        }));
    }

    if !update_available && !command.force {
        return Ok(Value::String(format!(
            "Already on latest version, {current}"
        )));
    }

    bail!(
        "this matrix install was not detected as Homebrew-managed; install or upgrade with: brew upgrade {MATRIX_HOMEBREW_FORMULA}"
    );
}

async fn maybe_print_update_notice(matrix: &Matrix, command: &Commands) {
    if matrix.output == OutputFormat::Json
        || matches!(command, Commands::Update(_))
        || env::var_os("MATRIX_NO_UPDATE_CHECK").is_some()
    {
        return;
    }
    if !io::stderr().is_terminal() {
        return;
    }
    let Ok(cache_path) = update_notice_cache_path() else {
        return;
    };
    if let Some(message) = fresh_update_notice(&cache_path) {
        if !message.is_empty() {
            eprintln!("{message}");
        }
        return;
    }
    let current = env!("CARGO_PKG_VERSION");
    let message = if is_homebrew_install() {
        homebrew_outdated_version()
            .map(|latest| format!("Update {latest} available, run `matrix update`\n"))
            .unwrap_or_default()
    } else {
        match latest_matrix_version(&matrix.client).await {
            Ok(latest) if version_is_newer(current, &latest) => {
                format!("Update {latest} available, run `matrix update`\n")
            }
            _ => String::new(),
        }
    };
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&cache_path, &message);
    if !message.is_empty() {
        eprintln!("{message}");
    }
}

async fn latest_matrix_version(client: &reqwest::Client) -> Result<String> {
    let mut request = client
        .get(format!(
            "https://api.github.com/repos/{MATRIX_REPOSITORY}/releases/latest"
        ))
        .timeout(Duration::from_secs(3));
    if let Some(token) = github_token() {
        request = request.bearer_auth(token);
    }
    let body: Value = request.send().await?.error_for_status()?.json().await?;
    body["tag_name"]
        .as_str()
        .map(normalize_version)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| anyhow!("latest release response did not include tag_name"))
}

fn github_token() -> Option<String> {
    ["MATRIX_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"]
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

fn fresh_update_notice(path: &std::path::Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.modified().ok()?.elapsed().ok()? > UPDATE_CHECK_TTL {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn update_notice_cache_path() -> Result<PathBuf> {
    Ok(cache_root()?.join("update_notice"))
}

fn clear_update_notice_cache() {
    if let Ok(path) = update_notice_cache_path() {
        let _ = fs::remove_file(path);
    }
}

fn is_homebrew_install() -> bool {
    let brew_has_formula = ProcessCommand::new("brew")
        .args(["list", "--formula", "matrix"])
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !brew_has_formula {
        return false;
    }
    env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| {
            let text = path.to_string_lossy();
            text.starts_with("/opt/homebrew/") || text.starts_with("/usr/local/")
        })
        .unwrap_or(false)
}

fn homebrew_installed_version() -> Option<String> {
    let output = ProcessCommand::new("brew")
        .args(["list", "--versions", "matrix"])
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.split_whitespace().nth(1).map(ToString::to_string)
}

fn homebrew_outdated_version() -> Option<String> {
    let output = ProcessCommand::new("brew")
        .args([
            "outdated",
            "--formula",
            "--verbose",
            MATRIX_HOMEBREW_FORMULA,
        ])
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    text.lines().find_map(|line| {
        let (_, latest) = line.split_once(" < ")?;
        latest.split_whitespace().next().map(ToString::to_string)
    })
}

fn run_status_command(command: &str, args: &[&str]) -> Result<()> {
    let status = ProcessCommand::new(command)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {command} {}", args.join(" ")))?;
    if !status.success() {
        bail!("{command} {} exited with {status}", args.join(" "));
    }
    Ok(())
}

fn version_is_newer(current: &str, latest: &str) -> bool {
    let current_parts = version_parts(current);
    let latest_parts = version_parts(latest);
    for index in 0..current_parts.len().max(latest_parts.len()) {
        let current_part = *current_parts.get(index).unwrap_or(&0);
        let latest_part = *latest_parts.get(index).unwrap_or(&0);
        if latest_part > current_part {
            return true;
        }
        if latest_part < current_part {
            return false;
        }
    }
    false
}

fn version_parts(version: &str) -> Vec<u64> {
    normalize_version(version)
        .split(['.', '-'])
        .take_while(|part| part.chars().all(|ch| ch.is_ascii_digit()))
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

fn normalize_version(version: &str) -> String {
    version
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
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

    fn detect_browsing(args: ContextArgs) -> Self {
        let zone_only_scope =
            args.zone.is_some() && args.repo.is_none() && args.component.is_none();
        let mut context = Self::detect(args);
        if zone_only_scope {
            context.repo = None;
            context.tag = None;
            context.sha = None;
            context.reference = None;
        }
        context
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

async fn get_fact(matrix: &Matrix, args: FactGetArgs) -> Result<Value> {
    let mut query = revision_selector_query(&args.selector)?;
    query.push(("limit", args.limit.max(1).to_string()));
    if args.history {
        query.push(("history", "true".to_string()));
    }
    matrix
        .get(&format!(
            "/facts/{}?{}",
            enc(&args.fact_id),
            query_string(query)
        ))
        .await
}

async fn history(matrix: &Matrix, args: HistoryArgs) -> Result<Value> {
    let mut query = revision_selector_query(&args.selector)?;
    query.push(("limit", args.limit.max(1).to_string()));
    matrix
        .get(&format!(
            "/facts/{}/history?{}",
            enc(&args.fact_id),
            query_string(query)
        ))
        .await
}

fn revision_selector_query(selector: &RevisionSelectorArgs) -> Result<Vec<(&'static str, String)>> {
    if selector.as_of.is_some()
        && (selector.revision.is_some() || selector.event.is_some() || selector.relative.is_some())
    {
        bail!("--as-of cannot be combined with --revision, --event, or --relative");
    }
    if selector.revision.is_some() && selector.event.is_some() {
        bail!("use only one of --revision or --event");
    }
    let mut query = Vec::new();
    if let Some(revision) = selector.revision {
        query.push(("revision", revision.to_string()));
    }
    if let Some(event) = selector.event.clone() {
        query.push(("eventId", event));
    }
    if let Some(relative) = selector.relative {
        query.push(("relative", relative.to_string()));
    }
    if let Some(as_of) = selector.as_of.clone() {
        query.push(("asOf", as_of));
    }
    Ok(query)
}

async fn upload(matrix: &Matrix, args: UploadArgs) -> Result<Value> {
    let body = read_input(args.file, args.stdin)?;
    matrix.request(Method::POST, "/facts", Some(body)).await
}

async fn ingest(matrix: &Matrix, args: IngestArgs) -> Result<Value> {
    let input = read_text_input(args.file.clone(), args.stdin)?;
    let adapter = ingest::normalize_adapter(&args.adapter)?;
    let repo = args.repo.clone().or_else(current_repo);
    let component = args
        .component
        .clone()
        .or_else(|| repo.as_deref().map(component_key))
        .unwrap_or_else(|| adapter.clone());
    let zone = args
        .zone
        .clone()
        .unwrap_or_else(|| ingest::default_zone(&adapter).to_string());
    let source = ingest::Source {
        adapter,
        zone,
        repo,
        component,
        version: args.version.clone(),
        sha: args.sha.clone().or_else(current_sha),
        reference: args
            .r#ref
            .clone()
            .or_else(current_exact_tag)
            .or_else(current_branch),
    };
    let facts = ingest::normalize(ingest::Request {
        adapter: args.adapter,
        input,
        source,
        junit_files: args.junit_files,
        junit_globs: args.junit_globs,
    })?;
    let fact = json!({ "facts": facts });
    if args.upload {
        matrix.request(Method::POST, "/facts", Some(fact)).await
    } else {
        Ok(fact)
    }
}

async fn query(matrix: &Matrix, args: QueryArgs) -> Result<Value> {
    let facts = fetch_facts(matrix, args.max_facts).await?;
    let context = MatrixContext::detect(args.context);
    let sql_init = matrix.sql_init()?;
    let db = build_facts_db_with_init(&facts, &context, sql_init.as_deref())?;
    execute_readonly_sql(&db, &args.sql)
}

#[derive(Clone, Copy)]
enum FactView {
    Deref,
    Members,
}

async fn fact_view_query(matrix: &Matrix, args: FactQueryArgs, view: FactView) -> Result<Value> {
    let facts = fetch_facts(matrix, args.max_facts).await?;
    let context = MatrixContext::detect(args.context);
    let sql_init = matrix.sql_init()?;
    let db = build_facts_db_with_init(&facts, &context, sql_init.as_deref())?;
    let sql = fact_view_sql(&args.fact_id, view);
    execute_readonly_sql(&db, &sql)
}

fn fact_view_sql(fact_id: &str, view: FactView) -> String {
    let fact_id = sql_literal(fact_id);
    match view {
        FactView::Deref => format!(
            "select edge, target, target_version, physical_chaincode, channel, network
             from deref
             where fact_id = {fact_id}
             order by case edge when 'member' then 0 when 'requires' then 1 when 'provides' then 2 else 3 end,
                      target"
        ),
        FactView::Members => format!(
            "select component, version, physical_chaincode, channel, network, services
             from members
             where fact_id = {fact_id}
             order by component"
        ),
    }
}

#[derive(Clone, Copy)]
enum ContextView {
    Upstream,
    Downstream,
    Compatible,
}

async fn list_components_query(matrix: &Matrix, args: ListQueryArgs) -> Result<Value> {
    let options = args.component_options();
    let context = MatrixContext::detect_browsing(args.context);
    let db = query_db(matrix, args.max_facts, &context).await?;
    let sql = components_query_sql(&db, &context, options, args.limit);
    execute_readonly_sql(&db, &sql)
}

async fn list_versions_query(matrix: &Matrix, args: VersionQueryArgs) -> Result<Value> {
    let options = args.component_options();
    let context = MatrixContext::detect_browsing(args.context);
    let db = query_db(matrix, args.max_facts, &context).await?;
    let sql = versions_query_sql(
        &db,
        &context,
        args.component_filter.as_deref(),
        options,
        args.limit,
    );
    execute_readonly_sql(&db, &sql)
}

async fn list_tags_query(matrix: &Matrix, args: ListQueryArgs) -> Result<Value> {
    let context = MatrixContext::detect_browsing(args.context);
    let db = query_db(matrix, args.max_facts, &context).await?;
    let sql = tags_query_sql(&db, &context, args.limit);
    execute_readonly_sql(&db, &sql)
}

async fn context_view_query(
    matrix: &Matrix,
    args: ListQueryArgs,
    view: ContextView,
) -> Result<Value> {
    let context = MatrixContext::detect_browsing(args.context);
    let db = query_db(matrix, args.max_facts, &context).await?;
    let sql = context_view_sql(view, args.limit);
    execute_readonly_sql(&db, &sql)
}

async fn compare_query(matrix: &Matrix, args: CompareArgs) -> Result<Value> {
    let context = MatrixContext::detect(args.context);
    let db = query_db(matrix, args.max_facts, &context).await?;
    let sql = compare_query_sql(&args.target, args.target_version.as_deref(), args.limit);
    execute_readonly_sql(&db, &sql)
}

async fn query_db(
    matrix: &Matrix,
    max_facts: usize,
    context: &MatrixContext,
) -> Result<Connection> {
    let facts = fetch_facts(matrix, max_facts).await?;
    let sql_init = matrix.sql_init()?;
    build_facts_db_with_init(&facts, context, sql_init.as_deref())
}

#[derive(Clone, Debug, Default)]
struct ComponentQueryOptions {
    all: bool,
    type_filter: Option<String>,
    include_applications: bool,
    include_dependencies: bool,
}

impl ListQueryArgs {
    fn component_options(&self) -> ComponentQueryOptions {
        ComponentQueryOptions {
            all: self.all,
            type_filter: self.type_filter.clone(),
            include_applications: self.include_applications,
            include_dependencies: self.include_dependencies,
        }
    }
}

impl VersionQueryArgs {
    fn component_options(&self) -> ComponentQueryOptions {
        ComponentQueryOptions {
            all: self.all,
            type_filter: self.type_filter.clone(),
            include_applications: self.include_applications,
            include_dependencies: self.include_dependencies,
        }
    }
}

fn components_query_sql(
    db: &Connection,
    context: &MatrixContext,
    options: ComponentQueryOptions,
    limit: usize,
) -> String {
    format!(
        "select row_number() over (order by max(observed_at) desc, canonical_component asc, subject_name asc) as pick,
                component, canonical_component, subject_name, identity, subject_class,
                type, repo, zone, count(*) as facts, max(observed_at) as last_observed_at
         from components
         where component is not null and component != '' {} {}
         group by component, canonical_component, subject_name, identity, subject_class, type, repo, zone
         order by last_observed_at desc, canonical_component asc, subject_name asc
         limit {}",
        context_filter_sql(db, context, "components", &["repo", "zone"]),
        component_subject_filter_sql(&options, context.component.is_some()),
        limit.max(1)
    )
}

fn versions_query_sql(
    db: &Connection,
    context: &MatrixContext,
    component_override: Option<&str>,
    options: ComponentQueryOptions,
    limit: usize,
) -> String {
    let component_filter = component_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" and {}", identity_match_sql(Some("components"), value)))
        .unwrap_or_default();
    format!(
        "select row_number() over (order by max(observed_at) desc, version desc) as pick,
                version, component, canonical_component, subject_name, identity, subject_class,
                type, repo, zone, status, count(*) as facts, max(observed_at) as last_observed_at
         from components
         where version is not null and component is not null and component != '' {} {} {}
         group by version, component, canonical_component, subject_name, identity, subject_class, type, repo, zone, status
         order by last_observed_at desc, version desc
         limit {}",
        context_filter_sql(db, context, "components", &["repo", "zone", "component"]),
        component_filter,
        component_subject_filter_sql(&options, context.component.is_some()),
        limit.max(1)
    )
}

fn component_subject_filter_sql(
    options: &ComponentQueryOptions,
    include_focused_application: bool,
) -> String {
    if let Some(subject_type) = options.type_filter.as_deref() {
        return format!(" and type = {}", sql_literal(subject_type));
    }
    if options.all {
        return String::new();
    }
    let mut filters = Vec::new();
    if !options.include_applications && !include_focused_application {
        filters.push("(subject_class is null or subject_class != 'application')".to_string());
    }
    if !options.include_dependencies {
        filters.push("(subject_class is null or subject_class != 'dependency')".to_string());
    }
    if filters.is_empty() {
        String::new()
    } else {
        format!(" and {}", filters.join(" and "))
    }
}

#[cfg(feature = "interactive")]
fn parse_component_query_options(args: Vec<&str>) -> Result<(ComponentQueryOptions, Vec<String>)> {
    let mut options = ComponentQueryOptions::default();
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--all" => options.all = true,
            "--include-applications" | "--applications" => options.include_applications = true,
            "--include-dependencies" | "--dependencies" => options.include_dependencies = true,
            "--type" | "-t" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    bail!("--type requires a value");
                };
                options.type_filter = Some((*value).to_string());
            }
            value if value.starts_with("--type=") => {
                options.type_filter = Some(value.trim_start_matches("--type=").to_string());
            }
            value => rest.push(value.to_string()),
        }
        index += 1;
    }
    Ok((options, rest))
}

#[cfg(feature = "interactive")]
fn parse_compare_repl_args(args: Vec<&str>) -> Result<(String, Option<String>)> {
    let mut target = Vec::new();
    let mut target_version = None;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--target-version" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    bail!("--target-version requires a value");
                };
                target_version = Some((*value).to_string());
            }
            value if value.starts_with("--target-version=") => {
                target_version = Some(value.trim_start_matches("--target-version=").to_string());
            }
            value => target.push(value.to_string()),
        }
        index += 1;
    }
    if target.is_empty() {
        bail!("usage: .compare <component|repo|subject> [--target-version <version>]");
    }
    Ok((target.join(" "), target_version))
}

#[cfg(feature = "interactive")]
fn parse_repl_history_args(args: Vec<&str>) -> Result<HistoryArgs> {
    let mut argv = vec!["matrix", "history"];
    argv.extend(args);
    match Cli::try_parse_from(argv)?.command {
        Commands::History(args) => Ok(args),
        _ => unreachable!("history argv should parse as history command"),
    }
}

#[cfg(feature = "interactive")]
fn parse_repl_get_args(args: Vec<&str>) -> Result<FactGetArgs> {
    let mut argv = vec!["matrix", "get"];
    argv.extend(args);
    match Cli::try_parse_from(argv)?.command {
        Commands::Get(args) => Ok(args),
        _ => unreachable!("get argv should parse as get command"),
    }
}

fn tags_query_sql(db: &Connection, context: &MatrixContext, limit: usize) -> String {
    format!(
        "select row_number() over (order by max(observed_at) desc, tag desc) as pick,
                tag, component, repo, zone, status, count(*) as facts, max(observed_at) as last_observed_at
         from facts
         where tag is not null {}
         group by tag, component, repo, zone, status
         order by last_observed_at desc, tag desc
         limit {}",
        context_filter_sql(db, context, "facts", &["repo", "zone", "component"]),
        limit.max(1)
    )
}

fn context_view_sql(view: ContextView, limit: usize) -> String {
    let limit = limit.max(1);
    match view {
        ContextView::Upstream => format!(
            "select current_component, current_version, capability, capability_version,
                    component, version, repo, zone, status
             from upstream
             order by capability, status, component, version
             limit {limit}"
        ),
        ContextView::Downstream => format!(
            "select current_component, current_version, capability, capability_version,
                    component, version, repo, zone, status
             from downstream
             order by capability, status, component, version
             limit {limit}"
        ),
        ContextView::Compatible => format!(
            "select current_component, current_version, capability, capability_version,
                    component, version, repo, zone, status
             from compatible_with_current
             order by component, version, capability
             limit {limit}"
        ),
    }
}

fn compare_query_sql(target: &str, target_version: Option<&str>, limit: usize) -> String {
    let limit = limit.max(1);
    let upstream_target = target_match_sql("u", target, target_version);
    let downstream_target = target_match_sql("d", target, target_version);
    format!(
        "select 'current_requires_target' as relationship,
                current_component, current_version,
                component as target_component, version as target_version,
                canonical_component as target_canonical_component,
                identity as target_identity, subject_name as target_subject,
                repo as target_repo, zone as target_zone, status,
                capability, capability_version
         from upstream u
         where {upstream_target}
         union all
         select 'target_requires_current' as relationship,
                current_component, current_version,
                component as target_component, version as target_version,
                canonical_component as target_canonical_component,
                identity as target_identity, subject_name as target_subject,
                repo as target_repo, zone as target_zone, status,
                capability, capability_version
         from downstream d
         where {downstream_target}
         order by relationship, capability, target_component, target_version
         limit {limit}"
    )
}

fn target_match_sql(alias: &str, target: &str, target_version: Option<&str>) -> String {
    let mut clauses = vec![identity_match_sql(Some(alias), target)];
    if let Some(version) = target_version {
        clauses.push(format!("{alias}.version = {}", sql_literal(version)));
    }
    clauses.join(" and ")
}

fn identity_match_sql(table: Option<&str>, target: &str) -> String {
    let target = target.trim();
    let exact = sql_literal(target);
    let key = sql_literal(&component_key(target));
    let (identity, component, subject_name, repo) = if let Some(table) = table {
        (
            format!("{table}.identity"),
            format!("{table}.component"),
            format!("{table}.subject_name"),
            format!("{table}.repo"),
        )
    } else {
        (
            "facts.identity".to_string(),
            "facts.component".to_string(),
            "facts.subject_name".to_string(),
            "facts.repo".to_string(),
        )
    };
    format!(
        "({component} = {key}
          or {component} = {exact}
          or {subject_name} = {exact}
          or {repo} = {exact}
          or {identity} = {exact}
          or exists (
            select 1 from identity_aliases matrix_alias
            where matrix_alias.identity = {identity}
              and (matrix_alias.alias = {exact} or matrix_alias.alias = {key})
          ))"
    )
}

fn context_filter_sql(
    db: &Connection,
    context: &MatrixContext,
    table: &str,
    fields: &[&str],
) -> String {
    let mut filters = Vec::new();
    if fields.contains(&"zone")
        && let Some(zone) = query_context_string(db, "zone").or_else(|| context.zone.clone())
    {
        filters.push(format!("{table}.zone = {}", sql_literal(&zone)));
    }
    if fields.contains(&"repo")
        && let Some(repo) = context.repo.as_deref()
    {
        filters.push(format!("{table}.repo = {}", sql_literal(repo)));
    }
    if fields.contains(&"component")
        && let Some(component) = context.component.as_deref()
    {
        filters.push(identity_match_sql(Some(table), component));
    }
    if filters.is_empty() {
        String::new()
    } else {
        format!(" and {}", filters.join(" and "))
    }
}

fn query_context_string(db: &Connection, field: &str) -> Option<String> {
    let sql = format!("select {field} from context limit 1");
    db.query_row(&sql, [], |row| row.get::<_, Option<String>>(0))
        .ok()
        .flatten()
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
    Human,
    Table,
    Json,
    Yaml,
    Csv,
}

#[cfg(feature = "interactive")]
struct ReplSession<'a> {
    matrix: &'a Matrix,
    context: MatrixContext,
    sql_init: Option<String>,
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
        let sql_init = matrix.sql_init()?;
        Ok(Self {
            matrix,
            db: build_facts_db_with_init(&facts, &context, sql_init.as_deref())?,
            context,
            sql_init,
            facts,
            choices: Vec::new(),
            max_facts,
            fact_count,
            output_mode: match matrix.output {
                OutputFormat::Json => OutputMode::Json,
                OutputFormat::Yaml => OutputMode::Yaml,
                OutputFormat::Csv => OutputMode::Csv,
                OutputFormat::Human => OutputMode::Human,
                OutputFormat::Table => OutputMode::Table,
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
        self.db = build_facts_db_with_init(&self.facts, &self.context, self.sql_init.as_deref())?;
        self.last_refresh = SystemTime::now();
        Ok(())
    }

    fn rebuild_context(&mut self) -> Result<()> {
        self.db = build_facts_db_with_init(&self.facts, &self.context, self.sql_init.as_deref())?;
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
                        if let Err(error) = self.run_sql(&sql) {
                            eprintln!("{error:#}");
                        }
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
            "components" => self.list_components(parts.collect::<Vec<_>>())?,
            "versions" => self.list_versions(parts.collect::<Vec<_>>())?,
            "tags" => self.list_tags()?,
            "use" => self.use_context_choice(parts.collect::<Vec<_>>())?,
            "status" => self.print_status()?,
            "tables" | "views" => self.run_sql(
                "select name, type from sqlite_master where type in ('table', 'view') order by type, name",
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
            "deref" => {
                let fact_id = parts.collect::<Vec<_>>().join(" ");
                if fact_id.is_empty() {
                    eprintln!("Usage: .deref <fact-id>");
                } else {
                    self.run_sql(&fact_view_sql(&fact_id, FactView::Deref))?;
                }
            }
            "members" => {
                let fact_id = parts.collect::<Vec<_>>().join(" ");
                if fact_id.is_empty() {
                    eprintln!("Usage: .members <fact-id>");
                } else {
                    self.run_sql(&fact_view_sql(&fact_id, FactView::Members))?;
                }
            }
            "get" => {
                let args = parts.collect::<Vec<_>>();
                if args.is_empty() {
                    eprintln!("Usage: .get <fact-id> [--revision N|--relative N|--event ID|--as-of DATE]");
                } else {
                    let args = parse_repl_get_args(args)?;
                    let value = get_fact(self.matrix, args).await?;
                    print_value(&value, self.matrix.output)?;
                }
            }
            "history" | "supersedes" => {
                let args = parts.collect::<Vec<_>>();
                if args.is_empty() {
                    eprintln!("Usage: .{name} <fact-id>");
                } else {
                    let args = parse_repl_history_args(args)?;
                    let value = history(self.matrix, args).await?;
                    print_value(&value, self.matrix.output)?;
                }
            }
            "compare" | "why" => {
                let args = parts.collect::<Vec<_>>();
                if args.is_empty() {
                    eprintln!("Usage: .{name} <component|repo|subject> [--target-version <version>]");
                } else {
                    let (target, target_version) = parse_compare_repl_args(args)?;
                    self.run_sql(&compare_query_sql(&target, target_version.as_deref(), 50))?;
                }
            }
            "examples" => print_repl_examples(),
            "describe" | "desc" | "d" => {
                let table = parts.next().unwrap_or("facts");
                self.run_sql(&format!(
                    "select * from pragma_table_info('{}')",
                    table.replace('\'', "''")
                ))?;
            }
            "mode" => match parts.next() {
                Some("human") => {
                    self.output_mode = OutputMode::Human;
                    eprintln!("Output mode: human");
                }
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
                    eprintln!("Unknown mode {other:?}; expected human, table, json, yaml, or csv.")
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

    fn list_components(&mut self, args: Vec<&str>) -> Result<()> {
        let (options, rest) = parse_component_query_options(args)?;
        if !rest.is_empty() {
            bail!(
                "usage: .components [--all] [--type <type>] [--include-applications] [--include-dependencies]"
            );
        }
        let sql = components_query_sql(&self.db, &self.context, options, 50);
        self.run_choice_sql("component", "component", &sql)
    }

    fn list_versions(&mut self, args: Vec<&str>) -> Result<()> {
        let (options, rest) = parse_component_query_options(args)?;
        let component_override = (!rest.is_empty()).then(|| rest.join(" "));
        let sql = versions_query_sql(
            &self.db,
            &self.context,
            component_override.as_deref(),
            options,
            50,
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
            filters.push(identity_match_sql(Some(table), component));
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

#[cfg(test)]
fn build_facts_db(facts: &[Value], context: &MatrixContext) -> Result<Connection> {
    build_facts_db_with_init(facts, context, None)
}

fn build_facts_db_with_init(
    facts: &[Value],
    context: &MatrixContext,
    sql_init: Option<&str>,
) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(
        "create table facts (
          id text, zone text, kind text, status text,
          type text, component text, canonical_component text, identity text,
          subject_class text, version text, repo text,
          source_repository text, source_repo text, source_sha text, source_ref text,
          subject_type text, subject_name text, channel text,
          tag text, observed_at text, accepted_at text,
          requires text, provides text, aliases text, json text not null
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
        let canonical_component = text_at(fact, &["canonicalComponent"])
            .or_else(|| text_at(fact, &["subject", "canonicalComponent"]))
            .or_else(|| component.clone());
        let subject_class = subject_class(subject_type.as_deref());
        let identity = text_at(fact, &["identity"])
            .or_else(|| text_at(fact, &["canonicalId"]))
            .or_else(|| text_at(fact, &["subject", "identity"]))
            .or_else(|| text_at(fact, &["subject", "id"]))
            .or_else(|| {
                subject_identity(
                    subject_type.as_deref(),
                    subject_name.as_deref(),
                    subject_repo.as_deref(),
                )
            });
        let aliases = subject_aliases_json(
            fact,
            subject_name.as_deref(),
            component.as_deref(),
            subject_repo.as_deref(),
            identity.as_deref(),
        )?;
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
            "insert into facts values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                text_at(fact, &["id"]).or_else(|| text_at(record, &["id"])),
                zone,
                text_at(fact, &["kind"]).or_else(|| text_at(record, &["kind"])),
                text_at(fact, &["status"]),
                subject_type.clone(),
                component,
                canonical_component,
                identity,
                subject_class,
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
                aliases,
                serde_json::to_string(record)?,
            ],
        )?;
    }
    create_matrix_views(&db, context, &zones)?;
    if let Some(sql) = sql_init {
        apply_sql_init(&db, sql)?;
    }
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
          select type, component, canonical_component, identity, subject_class, repo, count(*) as facts,
                 max(observed_at) as last_observed_at
          from facts
          where component is not null
          group by type, component, canonical_component, identity, subject_class, repo;
        create view components as
          select zone, type, subject_class, component, canonical_component, identity,
                 subject_name, repo, version, status,
                 case
                   when status in ('compatible', 'passed', 'observed', 'candidate', 'valid', 'ready') then 'valid'
                   when status in ('incompatible', 'failed', 'invalid', 'blocked') then 'invalid'
                   else coalesce(status, 'unknown')
                 end as status_class,
                 observed_at, accepted_at, id
          from facts
          where component is not null;
        create view identities as
          select identity, canonical_component, subject_class,
                 min(type) as type,
                 min(subject_name) as subject_name,
                 min(repo) as repo,
                 count(*) as facts,
                 max(observed_at) as last_observed_at
          from facts
          where identity is not null
          group by identity, canonical_component, subject_class;
        create view identity_aliases as
          select distinct identity, canonical_component, subject_class, type,
                 subject_name, repo, subject_name as alias, 'subject_name' as alias_kind
          from facts
          where identity is not null and subject_name is not null and subject_name != ''
          union
          select distinct identity, canonical_component, subject_class, type,
                 subject_name, repo, component as alias, 'component' as alias_kind
          from facts
          where identity is not null and component is not null and component != ''
          union
          select distinct identity, canonical_component, subject_class, type,
                 subject_name, repo, repo as alias, 'repo' as alias_kind
          from facts
          where identity is not null and repo is not null and repo != ''
          union
          select distinct f.identity, f.canonical_component, f.subject_class, f.type,
                 f.subject_name, f.repo, item.value as alias,
                 'alias' as alias_kind
          from facts f, json_each(coalesce(f.aliases, '[]')) item
          where f.identity is not null and item.value is not null
            and item.value != '';
        create view valid_facts as
          select * from facts
          where status in ('compatible', 'passed', 'observed', 'candidate', 'valid', 'ready');
        create view invalid_facts as
          select * from facts
          where status in ('incompatible', 'failed', 'invalid', 'blocked');
        create view requirements as
          select f.id as fact_id, f.zone, f.type, f.subject_class, f.component,
                 f.canonical_component, f.identity, f.subject_name, f.repo, f.version, f.status,
                 json_extract(item.value, '$.capability') as capability,
                 json_extract(item.value, '$.version') as capability_version,
                 item.value as requirement
          from facts f, json_each(coalesce(f.requires, '[]')) item;
        create view capabilities as
          select f.id as fact_id, f.zone, f.type, f.subject_class, f.component,
                 f.canonical_component, f.identity, f.subject_name, f.repo, f.version, f.status,
                 json_extract(item.value, '$.capability') as capability,
                 json_extract(item.value, '$.version') as capability_version,
                 item.value as provides
          from facts f, json_each(coalesce(f.provides, '[]')) item;
        create view members as
          select f.id as fact_id, f.zone, f.type as fact_type, f.component as fact_component,
                 f.repo as fact_repo, f.version as fact_version, f.status as fact_status,
                 json_extract(item.value, '$.component') as component,
                 json_extract(item.value, '$.version') as version,
                 json_extract(item.value, '$.logicalChaincode') as logical_chaincode,
                 json_extract(item.value, '$.physicalChaincode') as physical_chaincode,
                 json_extract(item.value, '$.chaincode') as chaincode,
                 json_extract(item.value, '$.channel') as channel,
                 json_extract(item.value, '$.network') as network,
                 json_extract(item.value, '$.digest') as digest,
                 json_extract(item.value, '$.services') as services,
                 json_extract(item.value, '$.aliases') as aliases,
                 item.value as member
          from facts f, json_each(coalesce(json_extract(f.json, '$.fact.members'), json_extract(f.json, '$.members'), '[]')) item;
        create view deref as
          select 'member' as edge, fact_id, zone, fact_type, fact_component, fact_repo,
                 fact_version, fact_status, 'component' as target_type,
                 component as target, version as target_version, null as capability,
                 logical_chaincode, physical_chaincode, channel, network, digest, services,
                 member as json
          from members
          union all
          select 'requires' as edge, fact_id, zone, type as fact_type, component as fact_component,
                 repo as fact_repo, version as fact_version, status as fact_status,
                 'capability' as target_type, capability as target,
                 capability_version as target_version, capability,
                 null as logical_chaincode, null as physical_chaincode, null as channel,
                 null as network, json_extract(requirement, '$.digest') as digest,
                 null as services, requirement as json
          from requirements
          union all
          select 'provides' as edge, fact_id, zone, type as fact_type, component as fact_component,
                 repo as fact_repo, version as fact_version, status as fact_status,
                 'capability' as target_type, capability as target,
                 capability_version as target_version, capability,
                 null as logical_chaincode, null as physical_chaincode, null as channel,
                 null as network, json_extract(provides, '$.digest') as digest,
                 null as services, provides as json
          from capabilities;",
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
         create view current as select * from active;
         create view zone as select * from facts where {zone_where};
         create view upstream as
           select min(r.fact_id) as current_fact_id,
                  r.zone as current_zone,
                  r.component as current_component,
                  r.repo as current_repo,
                  r.version as current_version,
                  r.capability,
                  r.capability_version,
                  min(p.fact_id) as provider_fact_id,
                  p.zone,
                  p.type,
                  p.subject_class,
                  p.component,
                  p.canonical_component,
                  p.identity,
                  p.subject_name,
                  p.repo,
                  p.version,
                  p.status,
                  min(p.provides) as provides
           from requirements r
           join active a on a.id = r.fact_id
           left join capabilities p
             on p.capability = r.capability
            and (r.capability_version is null or p.capability_version = r.capability_version)
           group by r.zone, r.component, r.repo, r.version, r.capability, r.capability_version,
                    p.zone, p.type, p.subject_class, p.component, p.canonical_component,
                    p.identity, p.subject_name, p.repo, p.version, p.status;
         create view downstream as
           select min(p.fact_id) as current_fact_id,
                  p.zone as current_zone,
                  p.component as current_component,
                  p.repo as current_repo,
                  p.version as current_version,
                  p.capability,
                  p.capability_version,
                  min(r.fact_id) as dependent_fact_id,
                  r.zone,
                  r.type,
                  r.subject_class,
                  r.component,
                  r.canonical_component,
                  r.identity,
                  r.subject_name,
                  r.repo,
                  r.version,
                  r.status,
                  min(r.requirement) as requirement
           from capabilities p
           join active a on a.id = p.fact_id
           left join requirements r
             on r.capability = p.capability
            and (p.capability_version is null or r.capability_version = p.capability_version)
           group by p.zone, p.component, p.repo, p.version, p.capability, p.capability_version,
                    r.zone, r.type, r.subject_class, r.component, r.canonical_component,
                    r.identity, r.subject_name, r.repo, r.version, r.status;
         create view compatible_with_current as
           select distinct * from downstream
           where status in ('compatible', 'passed', 'observed', 'candidate', 'valid', 'ready');",
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
                | "current"
                | "upstream"
                | "downstream"
                | "compatible_with_current"
                | "facts"
                | "context"
                | "subjects"
                | "components"
                | "identities"
                | "identity_aliases"
                | "valid_facts"
                | "invalid_facts"
                | "requirements"
                | "capabilities"
                | "members"
                | "deref"
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

fn apply_sql_init(db: &Connection, sql: &str) -> Result<()> {
    for statement in sql.split(';') {
        let statement = strip_sql_line_comments(statement)
            .trim()
            .to_ascii_lowercase();
        if statement.is_empty() {
            continue;
        }
        if !(statement.starts_with("create view ")
            || statement.starts_with("create temp view ")
            || statement.starts_with("create temporary view "))
        {
            bail!("Matrix SQL init only allows CREATE VIEW statements");
        }
    }
    db.execute_batch(sql)
        .context("failed to apply Matrix SQL init views")?;
    Ok(())
}

fn strip_sql_line_comments(sql: &str) -> String {
    sql.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("--")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_sql_pack_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
        .collect()
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
        filters.push(identity_match_sql(None, component));
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

fn subject_class(subject_type: Option<&str>) -> Option<String> {
    let subject_type = subject_type?;
    let normalized = subject_type.to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "app" | "application" | "repo" | "repository" | "sbom"
    ) {
        Some("application".to_string())
    } else if normalized.contains("dependency") {
        Some("dependency".to_string())
    } else {
        Some("component".to_string())
    }
}

fn subject_identity(
    subject_type: Option<&str>,
    subject_name: Option<&str>,
    repo: Option<&str>,
) -> Option<String> {
    let subject_type = subject_type
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("component")
        .to_ascii_lowercase();
    let subject_name = subject_name?.trim();
    if subject_name.is_empty() {
        return None;
    }
    if matches!(
        subject_type.as_str(),
        "app" | "application" | "repo" | "repository" | "sbom"
    ) && let Some(repo) = repo.filter(|value| !value.trim().is_empty())
    {
        return Some(format!(
            "{subject_type}:{}",
            repo.trim().to_ascii_lowercase()
        ));
    }
    Some(format!(
        "{subject_type}:{}",
        subject_name.to_ascii_lowercase()
    ))
}

fn subject_aliases_json(
    fact: &Value,
    subject_name: Option<&str>,
    component: Option<&str>,
    repo: Option<&str>,
    identity: Option<&str>,
) -> Result<Option<String>> {
    let mut aliases = std::collections::BTreeSet::new();
    for value in [subject_name, component, repo, identity]
        .into_iter()
        .flatten()
    {
        add_alias(&mut aliases, value);
    }
    collect_aliases(fact.get("aliases"), &mut aliases);
    collect_aliases(fact.pointer("/subject/aliases"), &mut aliases);
    if aliases.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(
            &aliases.into_iter().collect::<Vec<_>>(),
        )?))
    }
}

fn collect_aliases(value: Option<&Value>, aliases: &mut std::collections::BTreeSet<String>) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(alias) = item.as_str() {
                    add_alias(aliases, alias);
                }
            }
        }
        Some(Value::String(alias)) => add_alias(aliases, alias),
        _ => {}
    }
}

fn add_alias(aliases: &mut std::collections::BTreeSet<String>, alias: &str) {
    let alias = alias.trim();
    if !alias.is_empty() {
        aliases.insert(alias.to_string());
    }
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
        "capability",
        "repo",
        "version",
        "fact_id",
        "source_repo",
        "source_repository",
        "subject_name",
        "subject_type",
        "target",
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
        && !is_qualified_sql_identifier(token)
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

fn is_qualified_sql_identifier(token: &str) -> bool {
    let Some((qualifier, field)) = token.split_once('.') else {
        return false;
    };
    is_sql_identifier(qualifier) && is_sql_identifier(field) && !field.contains('.')
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
        OutputMode::Human => print_human_query_result(value)?,
        OutputMode::Json => print_json(value)?,
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
    let rows = query_rows(value).as_array().cloned().unwrap_or_default();
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
        Value::String(value) => compact_json_text(value).unwrap_or_else(|| value.clone()),
        Value::Bool(_) | Value::Number(_) => scalar_cell_value(value),
        Value::Array(values) => compact_array_value(values),
        Value::Object(object) => count_label(object.len(), "field"),
    }
}

fn table_cell_value(value: &Value) -> String {
    match value {
        Value::Null => "".to_string(),
        Value::String(value) => compact_json_text(value).unwrap_or_else(|| value.clone()),
        Value::Bool(value) => {
            if *value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::Array(values) => compact_array_value(values),
        Value::Object(object) => count_label(object.len(), "field"),
    }
}

fn compact_json_text(value: &str) -> Option<String> {
    let parsed = parse_nested_json_string(value)?;
    Some(match parsed {
        Value::Array(values) => compact_array_value(&values),
        Value::Object(object) => count_label(object.len(), "field"),
        other => table_cell_value(&other),
    })
}

fn compact_array_value(values: &[Value]) -> String {
    if values.is_empty() {
        return count_label(0, "item");
    }
    if values.iter().all(is_scalar_value) {
        return values
            .iter()
            .map(scalar_cell_value)
            .collect::<Vec<_>>()
            .join(", ");
    }
    count_label(values.len(), "item")
}

fn is_scalar_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::String(_) | Value::Bool(_) | Value::Number(_)
    )
}

fn scalar_cell_value(value: &Value) -> String {
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
        Value::Array(_) | Value::Object(_) => table_cell_value(value),
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
  .mode human|table|json|yaml|csv
                            Change output format
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
  .components [filters]     List primary components in the current context
  .versions [filters] [component]
                            List primary versions and remember picks for `.use`
  .tags                     List tags/refs and remember picks for `.use`
  .use <pick>               Focus a picked component, version, or tag
  .zones                    Summarize facts by zone
  .subjects                 Summarize facts by subject
  .trace <subject>          Show recent facts for a subject
  .gate <zone> [level]      Fetch a gate decision from the construct
  .views                    List local tables and views
  .get <fact-id>            Show the current or selected revision of a fact
  .deref <fact-id>          Show member/require/provide edges for a fact
  .members <fact-id>        Show tuple members for a fact
  .history <fact-id>        Show accepted revisions for a fact
  .history <fact-id> --relative -1
                            Show a selected revision by offset from current
  .compare <target>         Compare active context to a component/repo/subject
  .why <target>             Alias for .compare
  .examples                 Show copyable query examples
  .explain <sql>            Run EXPLAIN QUERY PLAN
  red, red-pill, .exit      Exit
  blue, blue-pill           Clear the current session context

SQL
  End SQL statements with `;`.
  Available tables/views: facts, active, zone, zones, subjects, components,
  identities, identity_aliases, current, upstream, downstream,
  compatible_with_current, valid_facts, invalid_facts, capabilities,
  requirements, members, deref, and one view per SQL-safe zone such as runtime.
  `status = valid` expands to compatible statuses.
  Component filters: --all, --type <type>, --include-applications,
  --include-dependencies.
"
    );
}

#[cfg(feature = "interactive")]
fn print_repl_examples() {
    println!(
        r#"Matrix SQL examples

select * from current;

select current_version, capability, component, version, status
from upstream;

select current_version, capability, component, version, status
from downstream;

select component, version, runtime, platform
from members
where fact_id==release-bundle.api.1.0.0;

select edge, target, target_version, runtime, platform
from deref
where fact_id==release-bundle.api.1.0.0;

.members release-bundle.api.1.0.0
.deref release-bundle.api.1.0.0
.history release-bundle.api.1.0.0 --relative -1
.history release-bundle.api.1.0.0 --as-of 2026-06-19
.compare ledger-service
.why example/ledger-service --target-version v2.4.0
"#
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
            ".deref",
            ".examples",
            ".explain",
            ".gate",
            ".get",
            ".help",
            ".history",
            ".limit",
            ".members",
            ".mode",
            ".refresh",
            ".schema",
            ".status",
            ".component",
            ".components",
            ".compare",
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
            ".views",
            ".why",
            ".zone",
            ".zones",
            "/help",
            "/status",
            "--as-of",
            "--event",
            "--relative",
            "--revision",
            "--target-version",
            "blue",
            "by",
            "capabilities",
            "capability",
            "channel",
            "compatible",
            "compatible_with_current",
            "component",
            "components",
            "compare",
            "context",
            "count",
            "csv",
            "current",
            "deref",
            "downstream",
            "facts",
            "fact_id",
            "from",
            "group",
            "get",
            "history",
            "id",
            "incompatible",
            "json",
            "kind",
            "limit",
            "members",
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
            "target",
            "target_version",
            "table",
            "tag",
            "tags",
            "type",
            "upstream",
            "use",
            "version",
            "versions",
            "where",
            "why",
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

fn read_text_input(file: Option<PathBuf>, stdin: bool) -> Result<String> {
    if stdin {
        return read_stdin_string();
    }
    if let Some(file) = file {
        return fs::read_to_string(&file)
            .with_context(|| format!("failed to read {}", file.display()));
    }
    bail!("provide a file path or --stdin")
}

fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "matrix", "matrix")
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dirs.config_dir().join("config.json"))
}

fn cache_root() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "matrix", "matrix")
        .ok_or_else(|| anyhow!("could not determine cache directory"))?;
    Ok(dirs.cache_dir().to_path_buf())
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
        OutputFormat::Json => print_json(value)?,
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
                print_human_query_result(value)?;
            } else {
                print_human_value(value);
            }
        }
    }
    Ok(())
}

fn print_json(value: &Value) -> Result<()> {
    let output = if is_query_result(value) {
        query_rows(value)
    } else {
        value.clone()
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn print_yaml(value: &Value) -> Result<()> {
    let output = if is_query_result(value) {
        query_rows(value)
    } else {
        value.clone()
    };
    print!("{}", serde_yaml::to_string(&output)?);
    Ok(())
}

fn is_query_result(value: &Value) -> bool {
    value.get("columns").is_some_and(Value::is_array)
        && value.get("rows").is_some_and(Value::is_array)
}

fn query_rows(value: &Value) -> Value {
    Value::Array(
        value["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(normalize_structured_value)
            .collect(),
    )
}

fn normalize_structured_value(value: Value) -> Value {
    match value {
        Value::String(text) => parse_nested_json_string(&text).unwrap_or(Value::String(text)),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(normalize_structured_value).collect())
        }
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, normalize_structured_value(value)))
                .collect(),
        ),
        other => other,
    }
}

fn parse_nested_json_string(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if !(trimmed.starts_with('[') || trimmed.starts_with('{')) {
        return None;
    }
    serde_json::from_str::<Value>(trimmed).ok()
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

fn print_human_query_result(value: &Value) -> Result<()> {
    print!("{}", human_query_result_text(value));
    Ok(())
}

fn human_query_result_text(value: &Value) -> String {
    let columns = value["columns"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(ToString::to_string))
        .collect::<Vec<_>>();
    let rows = query_rows(value).as_array().cloned().unwrap_or_default();

    if rows.is_empty() {
        return "No rows.\nTry `matrix components`, `matrix versions`, or `matrix query 'select * from context'` to inspect the active context.\n".to_string();
    }

    let mut text = format!(
        "{} row{}\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
    for (index, row) in rows.iter().enumerate() {
        let Some(object) = row.as_object() else {
            text.push_str(&format!("- {}\n", human_inline_value(row)));
            continue;
        };
        let (title, title_keys) = human_query_row_title(object, &columns, index);
        text.push_str(&format!("- {title}\n"));
        for column in &columns {
            if title_keys.iter().any(|key| key == column) {
                continue;
            }
            let Some(value) = object.get(column) else {
                continue;
            };
            text.push_str(&format!(
                "  {}: {}\n",
                human_label(column),
                human_inline_value(value)
            ));
        }
    }
    text
}

fn human_query_row_title(
    object: &serde_json::Map<String, Value>,
    columns: &[String],
    index: usize,
) -> (String, Vec<String>) {
    if let Some(component) = object.get("component").and_then(non_empty_human_value) {
        if let Some(version) = object.get("version").and_then(non_empty_human_value) {
            return (
                format!("{component} {version}"),
                vec!["component".to_string(), "version".to_string()],
            );
        }
        return (component, vec!["component".to_string()]);
    }
    for key in ["name", "id", "fact_id", "zone", "repo"] {
        if let Some(value) = object.get(key).and_then(non_empty_human_value) {
            return (value, vec![key.to_string()]);
        }
    }
    for column in columns {
        if let Some(value) = object.get(column).and_then(non_empty_human_value) {
            return (value, vec![column.clone()]);
        }
    }
    (format!("row {}", index + 1), Vec::new())
}

fn non_empty_human_value(value: &Value) -> Option<String> {
    let rendered = human_inline_value(value);
    if rendered.is_empty() || rendered == "-" {
        None
    } else {
        Some(rendered)
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
    if object.contains_key("factId") && object.contains_key("events") {
        print_fact_history_human(object);
        return;
    }
    if object.contains_key("factId") && object.contains_key("fact") {
        print_fact_get_human(object);
        return;
    }
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

fn print_fact_history_human(object: &serde_json::Map<String, Value>) {
    print!("{}", fact_history_human_text(object));
}

fn print_fact_get_human(object: &serde_json::Map<String, Value>) {
    print!("{}", fact_get_human_text(object));
}

fn fact_get_human_text(object: &serde_json::Map<String, Value>) -> String {
    let fact_id = object
        .get("factId")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event = object.get("event").and_then(Value::as_object);
    let fact = object.get("fact").and_then(Value::as_object);
    let mut text = format!("Fact: {fact_id}\n");
    if let Some(event) = event {
        push_object_field(&mut text, event, "revision", "Revision");
        push_object_field(&mut text, event, "status", "Revision status");
        push_object_field(&mut text, event, "acceptedAt", "Accepted");
        push_object_field(&mut text, event, "eventId", "Event");
        push_object_field(&mut text, event, "contentHash", "Hash");
    }
    if let Some(fact) = fact {
        text.push_str("Body\n");
        for key in [
            "id",
            "zone",
            "kind",
            "status",
            "sourceRepository",
            "sourceSha",
            "sourceRef",
        ] {
            push_object_field(&mut text, fact, key, &format!("  {}", human_label(key)));
        }
        if let Some(subject) = fact.get("subject").and_then(Value::as_object) {
            for key in ["type", "name", "version", "repo"] {
                push_object_field(
                    &mut text,
                    subject,
                    key,
                    &format!("  Subject {}", human_label(key)),
                );
            }
        }
    }
    text
}

fn fact_history_human_text(object: &serde_json::Map<String, Value>) -> String {
    let fact_id = object
        .get("factId")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let events = object
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut text = format!("Fact history: {fact_id}\n");
    if events.is_empty() {
        text.push_str("  no revisions\n");
        return text;
    }
    for event in events {
        let Some(event) = event.as_object() else {
            continue;
        };
        let revision = event
            .get("revision")
            .map(human_inline_value)
            .unwrap_or_else(|| "?".to_string());
        let status = event
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let accepted_at = event
            .get("acceptedAt")
            .and_then(Value::as_str)
            .unwrap_or("-");
        text.push_str(&format!("- r{revision} {status} accepted {accepted_at}\n"));
        push_object_field(&mut text, event, "eventId", "  Event");
        push_object_field(&mut text, event, "contentHash", "  Hash");
        push_object_field(&mut text, event, "sourceRepository", "  Source repo");
        push_object_field(&mut text, event, "sourceSha", "  Source SHA");
        push_object_field(&mut text, event, "sourceRef", "  Source ref");
        push_object_field(&mut text, event, "supersededAt", "  Superseded at");
        push_object_field(&mut text, event, "supersededBy", "  Superseded by");
    }
    text
}

fn print_object_field(object: &serde_json::Map<String, Value>, key: &str, label: &str) {
    if let Some(value) = object.get(key) {
        println!("{label}: {}", human_inline_value(value));
    }
}

fn push_object_field(
    text: &mut String,
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) {
    if let Some(value) = object.get(key) {
        text.push_str(&format!("{label}: {}\n", human_inline_value(value)));
    }
}

fn human_inline_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::String(value) => compact_json_text(value).unwrap_or_else(|| value.clone()),
        Value::Bool(value) => {
            if *value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::Array(values) => compact_array_value(values),
        Value::Object(_) => value.to_string(),
    }
}

fn count_label(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn human_label(key: &str) -> String {
    let mut label = String::new();
    for (index, character) in key.chars().enumerate() {
        if character == '_' || character == '-' {
            if !label.ends_with(' ') {
                label.push(' ');
            }
            continue;
        }
        if index > 0 && character.is_ascii_uppercase() && !label.ends_with(' ') {
            label.push(' ');
        }
        label.push(character);
    }
    let label = label.trim().to_string();
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
    const MAX_CELL_WIDTH: usize = 56;
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
    fn accepts_fact_view_commands() {
        let members = Cli::try_parse_from([
            "matrix",
            "members",
            "release-bundle.api.1.0.0",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(members.output, OutputFormat::Json);
        match members.command {
            Commands::Members(args) => {
                assert_eq!(args.fact_id, "release-bundle.api.1.0.0");
            }
            _ => panic!("expected members command"),
        }

        let deref = Cli::try_parse_from([
            "matrix",
            "deref",
            "release-bundle.api.1.0.0",
            "--max-facts",
            "10000",
        ])
        .unwrap();
        match deref.command {
            Commands::Deref(args) => {
                assert_eq!(args.fact_id, "release-bundle.api.1.0.0");
                assert_eq!(args.max_facts, 10000);
            }
            _ => panic!("expected deref command"),
        }
    }

    #[test]
    fn accepts_history_commands() {
        let history = Cli::try_parse_from([
            "matrix",
            "history",
            "release-bundle.api.1.0.0",
            "--limit",
            "10",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(history.output, OutputFormat::Json);
        match history.command {
            Commands::History(args) => {
                assert_eq!(args.fact_id, "release-bundle.api.1.0.0");
                assert_eq!(args.limit, 10);
            }
            _ => panic!("expected history command"),
        }

        let supersedes =
            Cli::try_parse_from(["matrix", "supersedes", "release-bundle.api.1.0.0"]).unwrap();
        match supersedes.command {
            Commands::Supersedes(args) => {
                assert_eq!(args.fact_id, "release-bundle.api.1.0.0");
                assert_eq!(args.limit, 25);
            }
            _ => panic!("expected supersedes command"),
        }

        let selected = Cli::try_parse_from([
            "matrix",
            "history",
            "release-bundle.api.1.0.0",
            "--relative",
            "-1",
            "--revision",
            "3",
            "--as-of",
            "2026-06-19",
        ])
        .unwrap();
        match selected.command {
            Commands::History(args) => {
                assert!(revision_selector_query(&args.selector).is_err());
            }
            _ => panic!("expected history command"),
        }

        let relative = Cli::try_parse_from([
            "matrix",
            "history",
            "release-bundle.api.1.0.0",
            "--relative",
            "-1",
            "--event",
            "event.abc",
        ])
        .unwrap();
        match relative.command {
            Commands::History(args) => {
                assert_eq!(args.selector.relative, Some(-1));
                assert_eq!(args.selector.event.as_deref(), Some("event.abc"));
            }
            _ => panic!("expected history command"),
        }

        let as_of = Cli::try_parse_from([
            "matrix",
            "history",
            "release-bundle.api.1.0.0",
            "--as-of",
            "2026-06-19T16:00:00Z",
        ])
        .unwrap();
        match as_of.command {
            Commands::History(args) => {
                assert_eq!(args.selector.as_of.as_deref(), Some("2026-06-19T16:00:00Z"));
            }
            _ => panic!("expected history command"),
        }

        let get = Cli::try_parse_from([
            "matrix",
            "get",
            "release-bundle.api.1.0.0",
            "--revision",
            "3",
            "--relative",
            "-1",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(get.output, OutputFormat::Json);
        match get.command {
            Commands::Get(args) => {
                assert_eq!(args.fact_id, "release-bundle.api.1.0.0");
                assert_eq!(args.selector.revision, Some(3));
                assert_eq!(args.selector.relative, Some(-1));
            }
            _ => panic!("expected get command"),
        }
    }

    #[test]
    fn accepts_tox_nox_junit_attachment_flags() {
        let cli = Cli::try_parse_from([
            "matrix",
            "ingest",
            "tox",
            "--file",
            "tox-result.json",
            "--junit-file",
            ".tox/py311/junit.xml",
            "--junit-glob",
            ".tox/*/junit.xml",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(cli.output, OutputFormat::Json);
        match cli.command {
            Commands::Ingest(args) => {
                assert_eq!(args.adapter, "tox");
                assert_eq!(
                    args.junit_files,
                    vec![PathBuf::from(".tox/py311/junit.xml")]
                );
                assert_eq!(args.junit_globs, vec![".tox/*/junit.xml"]);
            }
            _ => panic!("expected ingest command"),
        }
    }

    #[test]
    fn accepts_context_view_commands() {
        let compatible = Cli::try_parse_from([
            "matrix",
            "compatible",
            "--repo",
            "example/payments-api",
            "--version",
            "v0.6.3",
            "--limit",
            "25",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(compatible.output, OutputFormat::Json);
        match compatible.command {
            Commands::Compatible(args) => {
                assert_eq!(args.context.repo.as_deref(), Some("example/payments-api"));
                assert_eq!(args.context.version.as_deref(), Some("v0.6.3"));
                assert_eq!(args.limit, 25);
            }
            _ => panic!("expected compatible command"),
        }

        let versions = Cli::try_parse_from([
            "matrix",
            "versions",
            "payments-api",
            "--repo",
            "example/payments-api",
        ])
        .unwrap();
        match versions.command {
            Commands::Versions(args) => {
                assert_eq!(args.component_filter.as_deref(), Some("payments-api"));
                assert_eq!(args.context.repo.as_deref(), Some("example/payments-api"));
            }
            _ => panic!("expected versions command"),
        }

        let dependencies = Cli::try_parse_from([
            "matrix",
            "components",
            "--repo",
            "example/web-client",
            "--include-dependencies",
            "--type",
            "npm-dependency",
        ])
        .unwrap();
        match dependencies.command {
            Commands::Components(args) => {
                assert!(args.include_dependencies);
                assert_eq!(args.type_filter.as_deref(), Some("npm-dependency"));
            }
            _ => panic!("expected components command"),
        }

        let compare = Cli::try_parse_from([
            "matrix",
            "compare",
            "example/ledger-service",
            "--repo",
            "example/payments-api",
            "--version",
            "v0.6.3",
            "--target-version",
            "0.19.2",
        ])
        .unwrap();
        match compare.command {
            Commands::Compare(args) => {
                assert_eq!(args.target, "example/ledger-service");
                assert_eq!(args.context.repo.as_deref(), Some("example/payments-api"));
                assert_eq!(args.context.version.as_deref(), Some("v0.6.3"));
                assert_eq!(args.target_version.as_deref(), Some("0.19.2"));
            }
            _ => panic!("expected compare command"),
        }
    }

    #[test]
    fn renders_objects_as_field_value_tables() {
        let text = generic_table_text(&json!({
            "track": "runtime",
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
        assert!(text.contains("| runtime"));
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
            {"zone": "runtime", "facts": 3},
            {"zone": "agent-admin", "facts": 2}
        ]));
        assert!(text.contains("| zone"));
        assert!(text.contains("| facts"));
        assert!(text.contains("| runtime"));
        assert!(text.contains("| agent-admin"));
        assert!(text.contains("(2 rows)"));
    }

    #[test]
    fn renders_nested_scalar_fields_compactly() {
        let text = generic_table_text(&json!([
            {
                "component": "identity_contract",
                "services": ["did"],
                "aliases": ["did", "api"],
                "digest": "sha256:735ce2ccadf47e3098ab3c0c6ac682ff0573e214a4decb1ee2f9f93a4d5b9e72"
            },
            {
                "component": "profile_contract",
                "services": "[\"anoncreds\",\"csr\",\"query\",\"vc\"]",
                "aliases": []
            }
        ]));
        assert!(text.contains("| identity_contract"));
        assert!(text.contains("| did"));
        assert!(text.contains("| did, api"));
        assert!(text.contains("| anoncreds, csr, query, vc"));
        assert!(!text.contains("[\"anoncreds\""));
        assert!(text.contains("sha256:735ce2ccadf47e3098ab3c0c6ac682ff"));
        assert!(text.contains("..."));
        assert!(!text.contains("9f93a4d5b9e72"));
    }

    #[test]
    fn renders_fact_history_as_audit_records() {
        let value = json!({
            "factId": "release-bundle.api.1.0.0",
            "events": [
                {
                    "eventId": "event.current",
                    "revision": 2,
                    "acceptedAt": "2026-06-19T16:00:00Z",
                    "contentHash": "sha256:new",
                    "sourceRepository": "example/api",
                    "sourceSha": "222",
                    "status": "current"
                },
                {
                    "eventId": "event.old",
                    "revision": 1,
                    "acceptedAt": "2026-06-19T15:00:00Z",
                    "contentHash": "sha256:old",
                    "sourceRepository": "example/api",
                    "sourceSha": "111",
                    "status": "superseded",
                    "supersededBy": "event.current",
                    "supersededAt": "2026-06-19T16:00:00Z"
                }
            ]
        });
        let text = fact_history_human_text(value.as_object().unwrap());
        assert!(text.contains("Fact history: release-bundle.api.1.0.0"));
        assert!(text.contains("- r2 current accepted 2026-06-19T16:00:00Z"));
        assert!(text.contains("- r1 superseded accepted 2026-06-19T15:00:00Z"));
        assert!(text.contains("Superseded by: event.current"));
    }

    #[test]
    fn renders_fact_get_as_selected_fact() {
        let value = json!({
            "factId": "release-bundle.api.1.0.0",
            "event": {
                "eventId": "event.current",
                "revision": 2,
                "acceptedAt": "2026-06-19T16:00:00Z",
                "contentHash": "sha256:new",
                "status": "current"
            },
            "fact": {
                "id": "release-bundle.api.1.0.0",
                "zone": "runtime",
                "status": "passed",
                "sourceRepository": "example/api",
                "sourceSha": "222",
                "subject": {
                    "type": "release-bundle",
                    "name": "api",
                    "version": "1.0.0"
                }
            }
        });
        let text = fact_get_human_text(value.as_object().unwrap());
        assert!(text.contains("Fact: release-bundle.api.1.0.0"));
        assert!(text.contains("Revision: 2"));
        assert!(text.contains("Revision status: current"));
        assert!(text.contains("  Zone: runtime"));
        assert!(text.contains("  Subject Version: 1.0.0"));
    }

    #[test]
    fn renders_query_human_output_as_records() {
        let text = human_query_result_text(&json!({
            "columns": ["component", "version", "physical_chaincode", "services"],
            "rows": [
                {
                    "component": "identity_contract",
                    "version": "0.4.9",
                    "physical_chaincode": "identity_contract_v0_4_9",
                    "services": "[\"did\"]"
                },
                {
                    "component": "profile_contract",
                    "version": "0.2.7",
                    "physical_chaincode": "profile_contract_v0_2_7",
                    "services": "[\"anoncreds\",\"csr\"]"
                }
            ]
        }));
        assert!(text.starts_with("2 rows\n"));
        assert!(text.contains("- identity_contract 0.4.9\n"));
        assert!(text.contains("  Physical chaincode: identity_contract_v0_4_9\n"));
        assert!(text.contains("  Services: did\n"));
        assert!(text.contains("- profile_contract 0.2.7\n"));
        assert!(text.contains("  Services: anoncreds, csr\n"));
        assert!(!text.contains("+"));
        assert!(!text.contains("[\"anoncreds\""));
    }

    #[test]
    fn renders_csv_cells_as_spreadsheet_text() {
        assert_eq!(display_cell(&json!(["did", "api"])), "did, api");
        assert_eq!(display_cell(&json!("[\"did\",\"api\"]")), "did, api");
        assert_eq!(
            csv_escape(&display_cell(&json!(["did", "api"]))),
            "\"did, api\""
        );
        assert_eq!(display_cell(&json!({"requires": ["a", "b"]})), "1 field");
    }

    #[test]
    fn renders_query_structured_outputs_as_rows() {
        let value = json!({
            "columns": ["component", "services"],
            "rows": [
                {"component": "identity_contract", "services": "[\"did\"]"},
                {"component": "profile_contract", "services": "[\"anoncreds\",\"csr\"]"}
            ]
        });
        let rows = query_rows(&value);
        assert_eq!(
            rows,
            json!([
                {"component": "identity_contract", "services": ["did"]},
                {"component": "profile_contract", "services": ["anoncreds", "csr"]}
            ])
        );

        let yaml = serde_yaml::to_string(&rows).unwrap();
        assert!(yaml.starts_with("- component: identity_contract"));
        assert!(yaml.contains("services:\n  - did"));
        assert!(yaml.contains("- component: profile_contract"));
        assert!(yaml.contains("  - anoncreds\n  - csr"));
        assert!(!yaml.contains("columns:"));
        assert!(!yaml.contains("rows:"));
        assert!(!yaml.contains("'[\""));

        let json = serde_json::to_string_pretty(&rows).unwrap();
        assert!(json.starts_with("["));
        assert!(json.contains("\"services\": ["));
        assert!(!json.contains("\"columns\""));
        assert!(!json.contains("\"rows\""));
        assert!(!json.contains("\"[\\\""));
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
    fn compares_versions_without_tag_prefix() {
        assert!(version_is_newer("0.3.0", "v0.3.1"));
        assert!(version_is_newer("0.3.9", "0.4.0"));
        assert!(!version_is_newer("0.4.0", "0.3.9"));
        assert!(!version_is_newer("1.0.0", "V1.0.0"));
    }

    #[test]
    fn normalizes_short_matrix_sql() {
        assert_eq!(
            normalize_matrix_sql("select * from zone where type==chaincode and status==failed"),
            "select * from zone where type='chaincode' and status='failed'"
        );
        assert_eq!(
            normalize_matrix_sql(
                "select * from zone where repo==example/ledger-service and status!=failed"
            ),
            "select * from zone where repo='example/ledger-service' and status!='failed'"
        );
        assert_eq!(
            normalize_matrix_sql("select * from zone where type==chaincode and status==valid"),
            "select * from zone where type='chaincode' and status in ('compatible','passed','observed','candidate','valid','ready')"
        );
        assert_eq!(
            normalize_matrix_sql("select * from deref where fact_id==release-bundle.api.1.0.0"),
            "select * from deref where fact_id='release-bundle.api.1.0.0'"
        );
        assert_eq!(
            normalize_matrix_sql("select * from requirements r where r.fact_id = runtime.id"),
            "select * from requirements r where r.fact_id = runtime.id"
        );
    }

    #[test]
    fn derives_short_component_keys() {
        assert_eq!(component_key("@example/ledger-service"), "ledger-service");
        assert_eq!(component_key("identity_contract"), "identity_contract");
    }

    #[test]
    fn repo_override_does_not_inherit_git_source_context() {
        let context = MatrixContext::detect(ContextArgs {
            repo: Some("example/ledger-service".to_string()),
            ..ContextArgs::default()
        });
        assert_eq!(context.repo.as_deref(), Some("example/ledger-service"));
        assert!(context.sha.is_none());
        assert!(context.reference.is_none());
        assert!(context.tag.is_none());
    }

    #[test]
    fn zone_browsing_does_not_inherit_git_source_context() {
        let context = MatrixContext::detect_browsing(ContextArgs {
            zone: Some("runtime".to_string()),
            ..ContextArgs::default()
        });
        assert_eq!(context.zone.as_deref(), Some("runtime"));
        assert!(context.repo.is_none());
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
                repo: Some("example/payments-api".to_string()),
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
            vec![
                "--repo",
                "example/payments-api",
                "--target-version",
                "v0.6.3"
            ]
        );
    }

    #[test]
    fn creates_contextual_zone_view() {
        let facts = vec![
            json!({
                "id": "payments-api",
                "track": "runtime",
                "status": "candidate",
                "source": {"repo": "example/payments-api"},
                "subject": {"type": "npm", "name": "@example/payments-sdk", "version": "0.6.12", "repo": "example/payments-api"}
            }),
            json!({
                "id": "did",
                "track": "runtime",
                "status": "observed",
                "subject": {"type": "chaincode", "name": "identity_contract", "version": "0.4.8", "repo": "example/identity-contracts"}
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
                repo: Some("example/payments-api".to_string()),
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
        assert_eq!(result["rows"][0]["component"], "identity_contract");
    }

    #[test]
    fn exposes_one_shot_context_views() {
        let facts = vec![
            json!({
                "id": "payments-api-1",
                "zone": "runtime",
                "status": "valid",
                "subject": {"type": "app", "name": "payments-api", "version": "v0.6.3", "repo": "example/payments-api"},
                "observedAt": "2026-06-18T00:00:00Z",
                "provides": [{"capability": "cap.payments-api", "version": "1"}],
                "requires": [{"capability": "cap.auth-service", "version": "2"}]
            }),
            json!({
                "id": "auth-service-1",
                "zone": "runtime",
                "status": "valid",
                "subject": {"type": "service", "name": "auth-service", "version": "v1.2.0", "repo": "example/auth-service"},
                "observedAt": "2026-06-18T00:01:00Z",
                "provides": [{"capability": "cap.auth-service", "version": "2"}]
            }),
            json!({
                "id": "ledger-service-1",
                "zone": "runtime",
                "status": "valid",
                "subject": {"type": "service", "name": "ledger-service", "version": "v2.0.0", "repo": "example/ledger-service"},
                "observedAt": "2026-06-18T00:02:00Z",
                "requires": [{"capability": "cap.payments-api", "version": "1"}]
            }),
        ];
        let context = MatrixContext::detect(ContextArgs {
            zone: Some("runtime".to_string()),
            repo: Some("example/payments-api".to_string()),
            component: Some("payments-api".to_string()),
            version: Some("v0.6.3".to_string()),
            ..ContextArgs::default()
        });
        let db = build_facts_db_with_init(&facts, &context, None).unwrap();

        let components = execute_readonly_sql(
            &db,
            &components_query_sql(&db, &context, ComponentQueryOptions::default(), 10),
        )
        .unwrap();
        assert_eq!(components["rows"].as_array().unwrap().len(), 1);
        assert_eq!(components["rows"][0]["component"], "payments-api");

        let versions = execute_readonly_sql(
            &db,
            &versions_query_sql(
                &db,
                &context,
                Some("payments-api"),
                ComponentQueryOptions::default(),
                10,
            ),
        )
        .unwrap();
        assert_eq!(versions["rows"][0]["version"], "v0.6.3");

        let upstream =
            execute_readonly_sql(&db, &context_view_sql(ContextView::Upstream, 10)).unwrap();
        assert_eq!(upstream["rows"][0]["component"], "auth-service");

        let compatible =
            execute_readonly_sql(&db, &context_view_sql(ContextView::Compatible, 10)).unwrap();
        assert_eq!(compatible["rows"][0]["component"], "ledger-service");

        let compare_ledger_service =
            execute_readonly_sql(&db, &compare_query_sql("example/ledger-service", None, 10))
                .unwrap();
        assert_eq!(
            compare_ledger_service["rows"][0]["relationship"],
            "target_requires_current"
        );
        assert_eq!(
            compare_ledger_service["rows"][0]["target_component"],
            "ledger-service"
        );
        assert_eq!(
            compare_ledger_service["rows"][0]["capability"],
            "cap.payments-api"
        );

        let compare_auth_service =
            execute_readonly_sql(&db, &compare_query_sql("auth-service", Some("v1.2.0"), 10))
                .unwrap();
        assert_eq!(
            compare_auth_service["rows"][0]["relationship"],
            "current_requires_target"
        );
        assert_eq!(
            compare_auth_service["rows"][0]["target_subject"],
            "auth-service"
        );
        assert_eq!(
            compare_auth_service["rows"][0]["capability"],
            "cap.auth-service"
        );
    }

    #[test]
    fn filters_dependency_subjects_from_default_component_views() {
        let facts = vec![json!({
            "id": "dep-core",
            "zone": "runtime",
            "status": "observed",
            "subject": {
                "type": "npm-dependency",
                "name": "@credo-ts/core",
                "version": "0.6.3",
                "repo": "example/web-client"
            },
            "observedAt": "2026-06-18T00:00:00Z"
        })];
        let context = MatrixContext::detect(ContextArgs {
            repo: Some("example/web-client".to_string()),
            ..ContextArgs::default()
        });
        let db = build_facts_db_with_init(&facts, &context, None).unwrap();

        let default_components = execute_readonly_sql(
            &db,
            &components_query_sql(&db, &context, ComponentQueryOptions::default(), 10),
        )
        .unwrap();
        assert!(default_components["rows"].as_array().unwrap().is_empty());

        let dependency_components = execute_readonly_sql(
            &db,
            &components_query_sql(
                &db,
                &context,
                ComponentQueryOptions {
                    include_dependencies: true,
                    ..ComponentQueryOptions::default()
                },
                10,
            ),
        )
        .unwrap();
        assert_eq!(dependency_components["rows"][0]["component"], "core");
        assert_eq!(
            dependency_components["rows"][0]["subject_name"],
            "@credo-ts/core"
        );

        let typed_components = execute_readonly_sql(
            &db,
            &components_query_sql(
                &db,
                &context,
                ComponentQueryOptions {
                    type_filter: Some("npm-dependency".to_string()),
                    ..ComponentQueryOptions::default()
                },
                10,
            ),
        )
        .unwrap();
        assert_eq!(
            typed_components["rows"][0]["subject_name"],
            "@credo-ts/core"
        );
    }

    #[test]
    fn exposes_canonical_identity_and_alias_matching() {
        let facts = vec![json!({
            "id": "dep-core",
            "zone": "runtime",
            "status": "observed",
            "subject": {
                "type": "npm-dependency",
                "name": "@credo-ts/core",
                "version": "0.6.3",
                "repo": "example/web-client",
                "aliases": ["credo-core"]
            },
            "observedAt": "2026-06-18T00:00:00Z"
        })];
        let context = MatrixContext {
            component: Some("credo-core".to_string()),
            ..MatrixContext::default()
        };
        let db = build_facts_db_with_init(&facts, &context, None).unwrap();

        let identities = execute_readonly_sql(
            &db,
            "select identity, canonical_component, subject_class from identities",
        )
        .unwrap();
        assert_eq!(
            identities["rows"][0]["identity"],
            "npm-dependency:@credo-ts/core"
        );
        assert_eq!(identities["rows"][0]["canonical_component"], "core");
        assert_eq!(identities["rows"][0]["subject_class"], "dependency");

        let aliases = execute_readonly_sql(
            &db,
            "select alias from identity_aliases where identity = 'npm-dependency:@credo-ts/core' order by alias",
        )
        .unwrap();
        let alias_values = aliases["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["alias"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(alias_values.contains(&"@credo-ts/core"));
        assert!(alias_values.contains(&"core"));
        assert!(alias_values.contains(&"credo-core"));

        let active =
            execute_readonly_sql(&db, "select subject_name, identity from active").unwrap();
        assert_eq!(active["rows"].as_array().unwrap().len(), 1);
        assert_eq!(active["rows"][0]["subject_name"], "@credo-ts/core");
    }

    #[test]
    fn filters_active_context_by_version_and_component_key() {
        let facts = vec![
            json!({
                "id": "ledger-service-1",
                "track": "runtime",
                "status": "candidate",
                "subject": {"type": "npm", "name": "@example/ledger-service", "version": "0.19.1", "repo": "example/ledger-service"}
            }),
            json!({
                "id": "ledger-service-2",
                "track": "runtime",
                "status": "candidate",
                "subject": {"type": "npm", "name": "@example/ledger-service", "version": "0.19.2", "repo": "example/ledger-service"}
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                repo: Some("example/ledger-service".to_string()),
                component: Some("ledger-service".to_string()),
                version: Some("0.19.2".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let result =
            execute_readonly_sql(&db, "select id, component, version from active").unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["id"], "ledger-service-2");
        assert_eq!(result["rows"][0]["component"], "ledger-service");
    }

    #[test]
    fn flattens_construct_wrapped_facts() {
        let facts = vec![
            json!({
                "acceptedAt": "2026-06-17T21:49:24.064Z",
                "id": "chaincode.profile_contract.0.2.9.6d562df953ec",
                "kind": "CompatibilityFact",
                "source": {"repository": "example/contract-service"},
                "track": "runtime",
                "fact": {
                    "id": "chaincode.profile_contract.0.2.9.6d562df953ec",
                    "kind": "CompatibilityFact",
                    "observedAt": "2026-06-17T21:49:21.392Z",
                    "source": {
                        "ref": "refs/tags/v0.2.9",
                        "repo": "example/contract-service",
                        "sha": "6d562df953eca829f918a6ea956482f761dccba8"
                    },
                    "status": "candidate",
                    "subject": {
                        "name": "profile_contract",
                        "repo": "example/contract-service",
                        "type": "chaincode",
                        "version": "0.2.9"
                    },
                    "track": "runtime"
                }
            }),
            json!({
                "acceptedAt": "2026-06-17T21:49:24.133Z",
                "id": "validation.runtime.profile_contract.0.2.9",
                "kind": "ValidationFact",
                "track": "runtime",
                "fact": {
                    "id": "validation.runtime.profile_contract.0.2.9",
                    "kind": "ValidationFact",
                    "observedAt": "2026-06-17T21:49:21.392Z",
                    "source": {
                        "repo": "example/contract-service",
                        "sha": "6d562df953eca829f918a6ea956482f761dccba8"
                    },
                    "status": "not-run",
                    "track": "runtime"
                }
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                zone: Some("runtime".to_string()),
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
        assert_eq!(result["rows"][0]["component"], "profile_contract");
        assert_eq!(result["rows"][0]["version"], "0.2.9");
        assert_eq!(result["rows"][0]["repo"], "example/contract-service");
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
                "id": "auth-service",
                "track": "runtime",
                "status": "passed",
                "subject": {"type": "npm", "name": "@example/auth-service", "version": "1.2.3", "repo": "example/auth-service"},
                "provides": [{"capability": "native-askar", "version": "1.2.3"}]
            }),
            json!({
                "id": "ledger-service",
                "track": "runtime",
                "status": "candidate",
                "subject": {"type": "service", "name": "ledger-service", "version": "2.0.0", "repo": "example/ledger-service"},
                "requires": [{"capability": "native-askar", "version": "1.2.3"}]
            }),
        ];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                zone: Some("runtime".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let result = execute_readonly_sql(
            &db,
            "select id, component from runtime
             where repo==example/ledger-service
               and exists (
                 select 1 from requirements r
                 where r.fact_id = runtime.id
                   and r.capability in (
                     select p.capability from capabilities p
                     where p.repo==example/auth-service and p.status==passed
                   )
               )",
        )
        .unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["component"], "ledger-service");
    }

    #[test]
    fn exposes_context_aware_dependency_views() {
        let facts = vec![
            json!({
                "id": "auth-service",
                "track": "runtime",
                "status": "passed",
                "subject": {"type": "npm", "name": "@example/auth-service", "version": "1.2.3", "repo": "example/auth-service"},
                "provides": [{"capability": "native-askar", "version": "1.2.3"}]
            }),
            json!({
                "id": "ledger-service",
                "track": "runtime",
                "status": "candidate",
                "subject": {"type": "service", "name": "ledger-service", "version": "2.0.0", "repo": "example/ledger-service"},
                "requires": [{"capability": "native-askar", "version": "1.2.3"}]
            }),
        ];

        let ledger_service_db = build_facts_db(
            &facts,
            &MatrixContext {
                repo: Some("example/ledger-service".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let upstream = execute_readonly_sql(
            &ledger_service_db,
            "select component, version from upstream",
        )
        .unwrap();
        assert_eq!(upstream["rows"].as_array().unwrap().len(), 1);
        assert_eq!(upstream["rows"][0]["component"], "auth-service");

        let auth_service_db = build_facts_db(
            &facts,
            &MatrixContext {
                repo: Some("example/auth-service".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();
        let downstream = execute_readonly_sql(
            &auth_service_db,
            "select component, version, status from compatible_with_current",
        )
        .unwrap();
        assert_eq!(downstream["rows"].as_array().unwrap().len(), 1);
        assert_eq!(downstream["rows"][0]["component"], "ledger-service");
        assert_eq!(downstream["rows"][0]["status"], "candidate");
    }

    #[test]
    fn exposes_fact_dereference_views() {
        let facts = vec![json!({
            "id": "release-bundle.api.1.0.0",
            "kind": "CompatibilityFact",
            "track": "runtime",
            "status": "observed",
            "subject": {
                "type": "release-bundle",
                "name": "api",
                "version": "0.1.0",
                "repo": "example/matrix-facts"
            },
            "members": [
                {
                    "component": "identity_contract",
                    "version": "0.4.9",
                    "logicalChaincode": "identity_contract",
                    "physicalChaincode": "identity_contract_v0_4_9",
                    "channel": "dev",
                    "network": "obpcs",
                    "services": ["did"]
                }
            ],
            "requires": [{"capability": "chaincode:identity_contract", "version": "0.4.9"}],
            "provides": [{"capability": "release-bundle:api", "version": "0.1.0"}]
        })];
        let db = build_facts_db(
            &facts,
            &MatrixContext {
                zone: Some("runtime".to_string()),
                ..MatrixContext::default()
            },
        )
        .unwrap();

        let members = execute_readonly_sql(
            &db,
            "select component, version, physical_chaincode from members where fact_id==release-bundle.api.1.0.0",
        )
        .unwrap();
        assert_eq!(members["rows"].as_array().unwrap().len(), 1);
        assert_eq!(members["rows"][0]["component"], "identity_contract");
        assert_eq!(
            members["rows"][0]["physical_chaincode"],
            "identity_contract_v0_4_9"
        );

        let deref = execute_readonly_sql(
            &db,
            "select edge, target, target_version from deref where fact_id==release-bundle.api.1.0.0 order by edge",
        )
        .unwrap();
        let rows = deref["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|row| row["edge"] == "member"));
        assert!(rows.iter().any(|row| row["edge"] == "requires"));
        assert!(rows.iter().any(|row| row["edge"] == "provides"));

        let command_members = execute_readonly_sql(
            &db,
            &fact_view_sql("release-bundle.api.1.0.0", FactView::Members),
        )
        .unwrap();
        assert_eq!(command_members["rows"].as_array().unwrap().len(), 1);

        let command_deref = execute_readonly_sql(
            &db,
            &fact_view_sql("release-bundle.api.1.0.0", FactView::Deref),
        )
        .unwrap();
        assert_eq!(command_deref["rows"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn applies_custom_sql_init_views() {
        let facts = vec![json!({
            "id": "release-bundle.api.1.0.0",
            "kind": "CompatibilityFact",
            "track": "runtime",
            "status": "observed",
            "subject": {"type": "release-bundle", "name": "api", "version": "0.1.0"},
            "members": [
                {
                    "component": "identity_contract",
                    "version": "0.4.9",
                    "physicalChaincode": "identity_contract_v0_4_9"
                }
            ]
        })];
        let db = build_facts_db_with_init(
            &facts,
            &MatrixContext::default(),
            Some(
                "-- Matrix local shortcuts\n\
                 create view api_bundle as\n\
                 select component, version, physical_chaincode\n\
                 from members\n\
                 where fact_id = 'release-bundle.api.1.0.0';",
            ),
        )
        .unwrap();
        let result = execute_readonly_sql(&db, "select * from api_bundle").unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), 1);
        assert_eq!(result["rows"][0]["component"], "identity_contract");
    }

    #[test]
    fn parses_sql_pack_lists() {
        assert_eq!(
            parse_sql_pack_list("base.sql, runtime.sql,,team.sql"),
            vec!["base.sql", "runtime.sql", "team.sql"]
        );
    }

    #[test]
    fn applies_sql_pack_views() {
        let facts = vec![json!({
            "id": "release-bundle.api.1.0.0",
            "kind": "CompatibilityFact",
            "track": "runtime",
            "status": "observed",
            "subject": {"type": "release-bundle", "name": "api", "version": "0.1.0"},
            "members": [
                {
                    "component": "identity_contract",
                    "version": "0.4.9",
                    "physicalChaincode": "identity_contract_v0_4_9"
                }
            ],
            "provides": [{"capability": "release-bundle:api", "version": "0.1.0"}]
        })];
        let db = build_facts_db_with_init(
            &facts,
            &MatrixContext::default(),
            Some(
                "create view pack_members as select * from members;\n\
                 create view pack_edges as select * from deref;",
            ),
        )
        .unwrap();
        let members = execute_readonly_sql(&db, "select component from pack_members").unwrap();
        assert_eq!(members["rows"].as_array().unwrap().len(), 1);
        let edges = execute_readonly_sql(&db, "select edge from pack_edges").unwrap();
        assert_eq!(edges["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rejects_mutating_sql_init() {
        let result =
            build_facts_db_with_init(&[], &MatrixContext::default(), Some("drop table facts;"));
        assert!(result.is_err());
    }
}
