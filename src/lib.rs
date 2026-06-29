use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque, hash_map::DefaultHasher},
    env, fs,
    hash::{Hash, Hasher},
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
    process::{self, Command as ProcessCommand, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use directories::ProjectDirs;
use reqwest::{Method, StatusCode, header::CONTENT_TYPE};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod ingest;

const MATRIX_REPOSITORY: &str = "adrianmross/matrix";
const MATRIX_HOMEBREW_FORMULA: &str = "adrianmross/tap/matrix";
const MATRIX_LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";
const RED_WIZ_CONSTRUCT_URL: &str = "https://platform-api.red-wiz.stream";
const RED_WIZ_API_PREFIX: &str = "/v1/compatibility";
const RED_WIZ_TOKEN_COMMAND: &str = "wiz auth token --audience platform-api --format json";
const UPDATE_CHECK_TTL: Duration = Duration::from_secs(60 * 60 * 24);
const FACT_CACHE_STALE_AFTER: Duration = Duration::from_secs(60 * 60 * 24);
const DEFAULT_FACT_CACHE_MAX_FACTS: usize = 1000;
const MATRIX_GRAPHQL_SCHEMA: &str = r#"schema {
  query: Query
}

type Query {
  path(from: String, source: String, to: String, target: String, limit: Int): GraphPathAnswer!
  worksWith(left: String!, right: String!, limit: Int): GraphWorksWithAnswer!
  status(component: String, name: String, limit: Int): GraphStatusAnswer!
  versions(component: String!, for: String, forComponent: String, limit: Int): GraphVersionsForAnswer!
  resolve(name: String, component: String): GraphResolveAnswer!
  producers(limit: Int, staleDays: Int): ProducerInventory!
}

type GraphPathAnswer {
  kind: String!
  status: String!
  found: Boolean!
  confidence: String!
  source: Component
  target: Component
  recommended: GraphPath
  pathCount: Int!
  paths: [GraphPath!]!
  missing: [String!]!
}

type GraphWorksWithAnswer {
  kind: String!
  status: String!
  compatible: Boolean!
  confidence: String!
  direction: String!
  left: Component
  right: Component
  recommended: GraphPath
  pathCount: Int!
  paths: [GraphPath!]!
  reasons: [String!]!
  missing: [String!]!
}

type GraphVersionsForAnswer {
  kind: String!
  component: Component
  for: Component
  versions: [String!]!
  versionCandidates: [VersionCandidate!]!
}

type GraphStatusAnswer {
  kind: String!
  component: Component
  outgoing: [GraphEdge!]!
  incoming: [GraphEdge!]!
  outgoingCount: Int!
  incomingCount: Int!
}

type GraphResolveAnswer {
  kind: String!
  requested: String!
  name: String!
  version: String
  resolved: Component
  ambiguous: Boolean!
  matchCount: Int!
  matches: [ResolveMatch!]!
  warnings: [String!]!
}

type ProducerInventory {
  kind: String!
  summary: ProducerSummary!
  rows: [Producer!]!
}

type Component {
  requested: String
  key: String!
  component: String!
  version: String
  identity: String
  subjectName: String
  repo: String
  status: String
  lastObservedAt: String
}

type GraphPath {
  length: Int!
  score: Int!
  confidence: String!
  reasons: [String!]!
  nodes: [Component!]!
  edges: [GraphEdge!]!
}

type GraphEdge {
  from: Component
  to: Component
  relationship: String!
  capability: String
  capabilityVersion: String
  sourceVersion: String
  targetVersion: String
  sourceFactId: String
  targetFactId: String
  status: String
  observedAt: String
}

type VersionCandidate {
  version: String!
  score: Int!
  confidence: String!
  pathCount: Int!
}

type ProducerSummary {
  producers: Int!
  staleProducers: Int!
  facts: Int!
  invalidFacts: Int!
  sourceRepoFacts: Int!
  inferredSubjectRepoFacts: Int!
  unknownProducerFacts: Int!
  missingProducerMetadataFacts: Int!
  staleAfterDays: Int!
}

type Producer {
  pick: Int!
  producer: String!
  facts: Int!
  components: Int!
  zones: Int!
  invalid_facts: Int!
  source_repo_facts: Int!
  inferred_subject_repo_facts: Int!
  unknown_producer_facts: Int!
  producer_metadata: String!
  last_observed_at: String
  freshness: String!
}

type ResolveMatch {
  node: Component
  aliasKinds: [String!]!
  outgoingCount: Int!
  incomingCount: Int!
}
"#;

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
    #[arg(long, env = "MATRIX_PROFILE", value_enum, global = true)]
    profile: Option<ConfigProfile>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum ConfigProfile {
    RedWiz,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum CachePolicy {
    #[default]
    Auto,
    PreferCache,
    Refresh,
    Offline,
}

impl ConfigProfile {
    fn as_str(self) -> &'static str {
        match self {
            Self::RedWiz => "red-wiz",
        }
    }

    fn construct(self) -> &'static str {
        match self {
            Self::RedWiz => RED_WIZ_CONSTRUCT_URL,
        }
    }

    fn api_prefix(self) -> &'static str {
        match self {
            Self::RedWiz => RED_WIZ_API_PREFIX,
        }
    }

    fn token_command(self) -> Option<&'static str> {
        match self {
            Self::RedWiz => Some(RED_WIZ_TOKEN_COMMAND),
        }
    }
}

impl CachePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::PreferCache => "prefer-cache",
            Self::Refresh => "refresh",
            Self::Offline => "offline",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "hot-miss" | "hot-miss-refresh" => Ok(Self::Auto),
            "prefer-cache" | "cache" | "cache-first" | "local" => Ok(Self::PreferCache),
            "refresh" | "always-refresh" | "live" => Ok(Self::Refresh),
            "offline" => Ok(Self::Offline),
            _ => bail!(
                "unknown cache policy {value:?}; expected auto, prefer-cache, refresh, or offline"
            ),
        }
    }
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
    Compatible(CompatibleArgs),
    Compare(CompareArgs),
    Path(GraphPathArgs),
    WorksWith(GraphPairArgs),
    Status(GraphStatusArgs),
    Resolve(GraphResolveArgs),
    Why(GraphPairArgs),
    #[command(alias = "coverage")]
    Producers(ProducerInventoryArgs),
    #[command(alias = "graphql")]
    Graph(GraphQueryArgs),
    Artifacts(ArtifactListArgs),
    Validations(ValidationListArgs),
    Capabilities,
    Scopes,
    Scope {
        scope_id: String,
    },
    Providers {
        capability: String,
    },
    Requirements {
        artifact_id: String,
    },
    Consumers {
        artifact_id: String,
    },
    Blockers(BlockersArgs),
    Eligibility {
        track: String,
        environment: String,
    },
    Sync(SyncArgs),
    Cache(CacheCommand),
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
    #[arg(long)]
    install_path: Option<PathBuf>,
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
    Use { profile: ConfigProfile },
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
    #[arg(value_name = "SQL", required_unless_present = "file")]
    sql: Option<String>,
    #[arg(
        short = 'f',
        long = "file",
        value_name = "FILE",
        conflicts_with = "sql"
    )]
    file: Option<PathBuf>,
    #[arg(long)]
    max_facts: Option<usize>,
    #[command(flatten)]
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args)]
struct FactQueryArgs {
    fact_id: String,
    #[arg(long)]
    max_facts: Option<usize>,
    #[command(flatten)]
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct ListQueryArgs {
    #[arg(long)]
    max_facts: Option<usize>,
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
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct VersionQueryArgs {
    #[arg(value_name = "COMPONENT")]
    component_filter: Option<String>,
    #[arg(long = "for", value_name = "COMPONENT")]
    for_component: Option<String>,
    #[arg(long)]
    max_facts: Option<usize>,
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
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct CompareArgs {
    target: String,
    #[arg(long)]
    target_version: Option<String>,
    #[arg(long)]
    max_facts: Option<usize>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[command(flatten)]
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct CompatibleArgs {
    #[arg(value_name = "LEFT")]
    left: Option<String>,
    #[arg(value_name = "RIGHT")]
    right: Option<String>,
    #[arg(long)]
    max_facts: Option<usize>,
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
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct GraphPathArgs {
    source: String,
    target: String,
    #[arg(long)]
    max_facts: Option<usize>,
    #[arg(long, default_value_t = 5)]
    limit: usize,
    #[command(flatten)]
    cache: FactCacheArgs,
}

#[derive(Args, Clone)]
struct GraphPairArgs {
    left: String,
    right: String,
    #[arg(long)]
    max_facts: Option<usize>,
    #[arg(long, default_value_t = 5)]
    limit: usize,
    #[command(flatten)]
    cache: FactCacheArgs,
}

#[derive(Args, Clone)]
struct ProducerInventoryArgs {
    #[arg(long)]
    max_facts: Option<usize>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long, default_value_t = 14)]
    stale_days: i64,
    #[command(flatten)]
    cache: FactCacheArgs,
    #[command(flatten)]
    context: ContextArgs,
}

#[derive(Args, Clone)]
struct GraphStatusArgs {
    component: String,
    #[arg(long)]
    max_facts: Option<usize>,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[command(flatten)]
    cache: FactCacheArgs,
}

#[derive(Args, Clone)]
struct GraphQueryArgs {
    #[arg(value_name = "QUERY", required_unless_present_any = ["file", "schema"])]
    query: Option<String>,
    #[arg(
        short = 'f',
        long = "file",
        value_name = "FILE",
        conflicts_with = "query"
    )]
    file: Option<PathBuf>,
    #[arg(long)]
    max_facts: Option<usize>,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[arg(long = "var", value_name = "NAME=VALUE")]
    vars: Vec<String>,
    #[arg(long, help = "Print the native Matrix GraphQL schema")]
    schema: bool,
    #[command(flatten)]
    cache: FactCacheArgs,
}

#[derive(Args, Clone)]
struct GraphResolveArgs {
    component: String,
    #[arg(long)]
    max_facts: Option<usize>,
    #[command(flatten)]
    cache: FactCacheArgs,
}

#[derive(Args, Clone, Default)]
struct FactCacheArgs {
    #[arg(
        long,
        help = "Use the local fact cache without contacting the construct"
    )]
    offline: bool,
    #[arg(
        long = "refresh-cache",
        help = "Fetch fresh facts and replace the local cache"
    )]
    refresh_cache: bool,
}

#[derive(Args, Clone)]
struct SyncArgs {
    #[arg(long)]
    max_facts: Option<usize>,
}

#[derive(Args)]
struct CacheCommand {
    #[command(subcommand)]
    command: CacheSubcommand,
}

#[derive(Subcommand)]
enum CacheSubcommand {
    Status,
    Clear {
        #[arg(long)]
        all: bool,
    },
}

#[derive(Args, Clone)]
struct UploadArgs {
    file: Option<PathBuf>,
    #[arg(long)]
    stdin: bool,
}

#[derive(Args, Clone, Default)]
struct PageArgs {
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Args, Clone, Default)]
struct ArtifactListArgs {
    #[arg(long)]
    track: Option<String>,
    #[arg(long = "subject-type")]
    subject_type: Option<String>,
    #[arg(long = "subject-name")]
    subject_name: Option<String>,
    #[arg(long = "subject-repo")]
    subject_repo: Option<String>,
    #[arg(long)]
    status: Option<String>,
    #[command(flatten)]
    page: PageArgs,
}

#[derive(Args, Clone, Default)]
struct ValidationListArgs {
    #[arg(long)]
    track: Option<String>,
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    scope: Option<String>,
    #[command(flatten)]
    page: PageArgs,
}

#[derive(Args, Clone, Default)]
struct BlockersArgs {
    track: String,
    #[arg(long)]
    environment: Option<String>,
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
    profile: Option<ConfigProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    construct: Option<String>,
    api_prefix: Option<String>,
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_command: Option<String>,
    sql_init: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sql_packs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_policy: Option<CachePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_max_facts: Option<usize>,
}

#[derive(Clone)]
struct Matrix {
    config_path: PathBuf,
    config: Config,
    profile: Option<ConfigProfile>,
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

#[derive(Clone, Copy, Debug, Default)]
struct FactLoadOptions {
    policy: CachePolicy,
}

struct CachedDb {
    db: Connection,
    cache: FactCacheSummary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FactCacheMetadata {
    construct: Option<String>,
    api_prefix: String,
    profile: Option<ConfigProfile>,
    schema_version: u32,
    fetched_at_unix: u64,
    checked_at_unix: Option<u64>,
    fact_count: usize,
    max_facts: usize,
    head_digest: Option<String>,
    head_fact_count: Option<usize>,
    head_latest_accepted_at: Option<String>,
    head_latest_fact_id: Option<String>,
    head_latest_content_hash: Option<String>,
}

#[derive(Clone, Debug)]
struct FactCacheSummary {
    source: FactCacheSource,
    policy: CachePolicy,
    path: PathBuf,
    metadata: Option<FactCacheMetadata>,
    age_seconds: Option<u64>,
    checked_age_seconds: Option<u64>,
    stale: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FactsHead {
    kind: String,
    schema_version: u32,
    fact_count: usize,
    digest: String,
    generated_at: Option<String>,
    latest_accepted_at: Option<String>,
    latest_fact_id: Option<String>,
    latest_content_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FactCacheSource {
    Live,
    Cache,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthCandidate {
    EnvToken(String),
    ConfigToken(String),
    EnvTokenFile(String),
    ConfigTokenFile(String),
    EnvTokenCommand(String),
    ConfigTokenCommand(String),
    ProfileTokenCommand(String),
}

impl AuthCandidate {
    fn source(&self) -> &'static str {
        match self {
            Self::EnvToken(_) => "env-token",
            Self::ConfigToken(_) => "config-token",
            Self::EnvTokenFile(_) => "env-token-file",
            Self::ConfigTokenFile(_) => "config-token-file",
            Self::EnvTokenCommand(_) => "env-token-command",
            Self::ConfigTokenCommand(_) => "config-token-command",
            Self::ProfileTokenCommand(_) => "profile-token-command",
        }
    }

    fn token_file(&self) -> Option<&str> {
        match self {
            Self::EnvTokenFile(path) | Self::ConfigTokenFile(path) => Some(path),
            _ => None,
        }
    }

    fn token_command(&self) -> Option<&str> {
        match self {
            Self::EnvTokenCommand(command)
            | Self::ConfigTokenCommand(command)
            | Self::ProfileTokenCommand(command) => Some(command),
            _ => None,
        }
    }
}

pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    if let Commands::Completion { shell } = cli.command {
        let mut command = Cli::command();
        generate(shell, &mut command, "matrix", &mut io::stdout());
        return Ok(());
    }
    let output_format = OutputFormat::from_cli(cli.output, cli.json);
    let mut matrix = Matrix::load(cli.construct, cli.api_prefix, cli.profile, output_format)?;
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
        Commands::Compatible(args) => compatible_command(&matrix, args).await?,
        Commands::Compare(args) => compare_query(&matrix, args).await?,
        Commands::Path(args) => graph_path_command(&matrix, args).await?,
        Commands::WorksWith(args) => works_with_command(&matrix, args).await?,
        Commands::Status(args) => graph_status_command(&matrix, args).await?,
        Commands::Resolve(args) => graph_resolve_command(&matrix, args).await?,
        Commands::Why(args) => graph_why_command(&matrix, args).await?,
        Commands::Producers(args) => producer_inventory_command(&matrix, args).await?,
        Commands::Graph(args) => graph_query_command(&matrix, args).await?,
        Commands::Artifacts(args) => list_artifacts(&matrix, args).await?,
        Commands::Validations(args) => list_validations(&matrix, args).await?,
        Commands::Capabilities => matrix.get("/capabilities").await?,
        Commands::Scopes => matrix.get("/scopes").await?,
        Commands::Scope { scope_id } => matrix.get(&format!("/scopes/{}", enc(&scope_id))).await?,
        Commands::Providers { capability } => {
            matrix
                .get(&format!("/capabilities/{}/providers", enc(&capability)))
                .await?
        }
        Commands::Requirements { artifact_id } => {
            matrix
                .get(&format!("/artifacts/{}/requirements", enc(&artifact_id)))
                .await?
        }
        Commands::Consumers { artifact_id } => {
            matrix
                .get(&format!("/artifacts/{}/consumers", enc(&artifact_id)))
                .await?
        }
        Commands::Blockers(args) => blockers(&matrix, args).await?,
        Commands::Eligibility { track, environment } => {
            matrix
                .get(&format!(
                    "/tracks/{}/environments/{}/eligibility",
                    enc(&track),
                    enc(&environment)
                ))
                .await?
        }
        Commands::Sync(args) => sync_cache(&matrix, args).await?,
        Commands::Cache(command) => cache_command(&matrix, command).await?,
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
        #[arg(long, env = "MATRIX_PROFILE", value_enum)]
        profile: Option<ConfigProfile>,
        #[arg(short = 'o', long = "out", env = "MATRIX_OUTPUT", value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
        #[arg(long, hide = true)]
        json: bool,
        #[command(flatten)]
        context: EnterContextArgs,
    }

    let cli = EnterCli::parse();
    let output_format = OutputFormat::from_cli(cli.output, cli.json);
    let matrix = Matrix::load(cli.construct, cli.api_prefix, cli.profile, output_format)?;
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
        profile_override: Option<ConfigProfile>,
        output: OutputFormat,
    ) -> Result<Self> {
        let config_path = config_path()?;
        let config = if config_path.exists() {
            serde_json::from_slice(&fs::read(&config_path)?)
                .with_context(|| format!("failed to parse {}", config_path.display()))?
        } else {
            Config::default()
        };
        let profile = profile_override
            .or_else(|| {
                env::var("MATRIX_PROFILE")
                    .ok()
                    .and_then(|value| ConfigProfile::from_str(&value, true).ok())
            })
            .or(config.profile);
        let construct = construct_override
            .or_else(|| env::var("MATRIX_CONSTRUCT_URL").ok())
            .or_else(|| profile.map(|profile| profile.construct().to_string()))
            .or_else(|| config.construct.clone())
            .map(|value| value.trim_end_matches('/').to_string());
        let api_prefix = prefix_override
            .or_else(|| env::var("MATRIX_API_PREFIX").ok())
            .or_else(|| profile.map(|profile| profile.api_prefix().to_string()))
            .or_else(|| config.api_prefix.clone())
            .unwrap_or_else(|| "/v1/matrix".to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            config_path,
            config,
            profile,
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

    fn cache_policy(&self) -> Result<CachePolicy> {
        if let Ok(value) = env::var("MATRIX_CACHE_POLICY") {
            return CachePolicy::parse(&value);
        }
        Ok(self.config.cache_policy.unwrap_or_default())
    }

    fn max_facts(&self, override_value: Option<usize>) -> Result<usize> {
        if let Some(value) = override_value {
            return Ok(value);
        }
        if let Ok(value) = env::var("MATRIX_MAX_FACTS") {
            return value
                .parse::<usize>()
                .with_context(|| format!("invalid MATRIX_MAX_FACTS value {value:?}"));
        }
        Ok(self
            .config
            .cache_max_facts
            .unwrap_or(DEFAULT_FACT_CACHE_MAX_FACTS))
    }

    fn fact_load_options(&self, args: &FactCacheArgs) -> Result<FactLoadOptions> {
        if args.offline && args.refresh_cache {
            bail!("--offline cannot be combined with --refresh-cache");
        }
        let policy = if args.offline {
            CachePolicy::Offline
        } else if args.refresh_cache {
            CachePolicy::Refresh
        } else {
            self.cache_policy()?
        };
        Ok(FactLoadOptions { policy })
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    async fn get_optional(&self, path: &str) -> Result<Option<Value>> {
        self.request_optional(Method::GET, path, None).await
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
        if let Some(token) = self.auth_token()? {
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

    async fn request_optional(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        let url = format!("{}{}{}", self.construct()?, self.api_prefix, path);
        let mut request = self.client.request(method, &url);
        if let Some(token) = self.auth_token()? {
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
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status.is_success() && looks_like_html(&content_type, &text) {
            bail!(
                "received HTML from the construct; authenticate or use a machine/API construct URL"
            );
        }
        let value = parse_response_text(&text);
        if !status.is_success() {
            bail!("construct {status}: {}", error_detail(&value, &text));
        }
        Ok(Some(value))
    }

    fn auth_candidate(&self) -> Option<AuthCandidate> {
        if let Some(token) = env::var("MATRIX_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            return Some(AuthCandidate::EnvToken(token));
        }
        if let Some(token) = self
            .config
            .token
            .clone()
            .filter(|value| !value.trim().is_empty())
        {
            return Some(AuthCandidate::ConfigToken(token));
        }
        if let Some(path) = env::var("MATRIX_TOKEN_FILE")
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            return Some(AuthCandidate::EnvTokenFile(path));
        }
        if let Some(path) = self.config.token_file.clone() {
            return Some(AuthCandidate::ConfigTokenFile(path));
        }
        if let Some(command) = env::var("MATRIX_TOKEN_COMMAND")
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            return Some(AuthCandidate::EnvTokenCommand(command));
        }
        if let Some(command) = self.config.token_command.clone() {
            return Some(AuthCandidate::ConfigTokenCommand(command));
        }
        self.profile
            .and_then(ConfigProfile::token_command)
            .map(str::to_string)
            .map(AuthCandidate::ProfileTokenCommand)
    }

    fn resolve_auth_candidate(&self, candidate: &AuthCandidate) -> Result<Option<String>> {
        match candidate {
            AuthCandidate::EnvToken(token) | AuthCandidate::ConfigToken(token) => {
                Ok(Some(token.clone()))
            }
            AuthCandidate::EnvTokenFile(path) | AuthCandidate::ConfigTokenFile(path) => {
                let token = fs::read_to_string(path)
                    .with_context(|| format!("failed to read Matrix token file {path}"))?
                    .trim()
                    .to_string();
                if !token.is_empty() {
                    return Ok(Some(token));
                }
                Ok(None)
            }
            AuthCandidate::EnvTokenCommand(command)
            | AuthCandidate::ConfigTokenCommand(command)
            | AuthCandidate::ProfileTokenCommand(command) => {
                let output = ProcessCommand::new("sh")
                    .arg("-c")
                    .arg(command)
                    .output()
                    .with_context(|| format!("failed to run Matrix token command {command:?}"))?;
                if !output.status.success() {
                    bail!(
                        "Matrix token command {command:?} exited with {}",
                        output.status
                    );
                }
                if let Some(token) = token_from_command_stdout(command, output.stdout)? {
                    return Ok(Some(token));
                }
                Ok(None)
            }
        }
    }

    fn auth_token(&self) -> Result<Option<String>> {
        if let Some(candidate) = self.auth_candidate() {
            return self.resolve_auth_candidate(&candidate);
        }
        Ok(None)
    }
}

fn token_from_command_stdout(command: &str, stdout: Vec<u8>) -> Result<Option<String>> {
    let text = String::from_utf8(stdout).context("Matrix token command did not emit UTF-8")?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return token_from_json_value(&value).map(Some).ok_or_else(|| {
            anyhow!(
                "Matrix token command {command:?} emitted JSON without a token; expected access_token, token, bearerToken, or bearer_token"
            )
        });
    }
    Ok(Some(text.to_string()))
}

fn token_from_json_value(value: &Value) -> Option<String> {
    for key in ["access_token", "token", "bearerToken", "bearer_token"] {
        if let Some(token) = value
            .get(key)
            .and_then(Value::as_str)
            .and_then(usable_token)
        {
            return Some(token);
        }
    }
    None
}

fn usable_token(value: &str) -> Option<String> {
    let token = value.trim();
    if token.is_empty() || token == "<token>" {
        None
    } else {
        Some(token.to_string())
    }
}

async fn config_command(matrix: &mut Matrix, command: ConfigCommand) -> Result<Value> {
    match command.command {
        ConfigSubcommand::List => Ok(json!({
            "configPath": matrix.config_path,
            "profile": matrix.config.profile.map(ConfigProfile::as_str),
            "construct": matrix.config.construct,
            "apiPrefix": matrix.config.api_prefix,
            "hasToken": matrix.config.token.is_some(),
            "tokenFile": matrix.config.token_file,
            "hasTokenCommand": matrix.config.token_command.is_some(),
            "sqlInit": matrix.config.sql_init,
            "sqlPacks": matrix.config.sql_packs,
            "cachePolicy": matrix.config.cache_policy.map(CachePolicy::as_str),
            "cacheMaxFacts": matrix.config.cache_max_facts,
        })),
        ConfigSubcommand::Get { key } => match key.as_str() {
            "profile" => Ok(json!({"profile": matrix.config.profile.map(ConfigProfile::as_str)})),
            "construct" => Ok(json!({"construct": matrix.config.construct})),
            "api-prefix" | "apiPrefix" => Ok(json!({"apiPrefix": matrix.config.api_prefix})),
            "token" => Ok(json!({"hasToken": matrix.config.token.is_some()})),
            "token-file" | "tokenFile" => Ok(json!({"tokenFile": matrix.config.token_file})),
            "token-command" | "tokenCommand" => {
                Ok(json!({"hasTokenCommand": matrix.config.token_command.is_some()}))
            }
            "sql-init" | "sqlInit" => Ok(json!({"sqlInit": matrix.config.sql_init})),
            "sql-pack" | "sql-packs" | "sqlPack" | "sqlPacks" => {
                Ok(json!({"sqlPacks": matrix.config.sql_packs}))
            }
            "cache-policy" | "cachePolicy" => {
                Ok(json!({"cachePolicy": matrix.config.cache_policy.map(CachePolicy::as_str)}))
            }
            "cache-max-facts" | "cacheMaxFacts" | "max-facts" | "maxFacts" => {
                Ok(json!({"cacheMaxFacts": matrix.config.cache_max_facts}))
            }
            _ => bail!(
                "unknown config key {key:?}; expected profile, construct, api-prefix, token, token-file, token-command, sql-init, sql-pack, sql-packs, cache-policy, or cache-max-facts"
            ),
        },
        ConfigSubcommand::Set { key, value } => {
            match key.as_str() {
                "profile" => {
                    matrix.config.profile = Some(
                        ConfigProfile::from_str(&value, true)
                            .map_err(|_| anyhow!("unknown profile {value:?}; expected red-wiz"))?,
                    )
                }
                "construct" => matrix.config.construct = Some(value),
                "api-prefix" | "apiPrefix" => matrix.config.api_prefix = Some(value),
                "token" => matrix.config.token = Some(value),
                "token-file" | "tokenFile" => matrix.config.token_file = Some(value),
                "token-command" | "tokenCommand" => matrix.config.token_command = Some(value),
                "sql-init" | "sqlInit" => matrix.config.sql_init = Some(value),
                "sql-pack" | "sqlPack" => matrix.config.sql_packs = vec![value],
                "sql-packs" | "sqlPacks" => matrix.config.sql_packs = parse_sql_pack_list(&value),
                "cache-policy" | "cachePolicy" => {
                    matrix.config.cache_policy = Some(CachePolicy::parse(&value)?)
                }
                "cache-max-facts" | "cacheMaxFacts" | "max-facts" | "maxFacts" => {
                    matrix.config.cache_max_facts = Some(
                        value
                            .parse::<usize>()
                            .with_context(|| format!("invalid cache max facts {value:?}"))?,
                    )
                }
                _ => bail!(
                    "unknown config key {key:?}; expected profile, construct, api-prefix, token, token-file, token-command, sql-init, sql-pack, sql-packs, cache-policy, or cache-max-facts"
                ),
            }
            matrix.save()?;
            Ok(json!({"saved": matrix.config_path}))
        }
        ConfigSubcommand::Use { profile } => {
            matrix.config.profile = Some(profile);
            matrix.config.construct = Some(profile.construct().to_string());
            matrix.config.api_prefix = Some(profile.api_prefix().to_string());
            matrix.config.token = None;
            matrix.config.token_file = None;
            matrix.config.token_command = profile.token_command().map(str::to_string);
            matrix.save()?;
            Ok(json!({
                "saved": matrix.config_path,
                "profile": profile.as_str(),
                "construct": profile.construct(),
                "apiPrefix": profile.api_prefix(),
                "hasToken": matrix.config.token.is_some(),
                "tokenFile": matrix.config.token_file,
                "hasTokenCommand": matrix.config.token_command.is_some(),
            }))
        }
    }
}

async fn update_command(matrix: &Matrix, command: UpdateCommand) -> Result<Value> {
    let homebrew_install = is_homebrew_install();
    if homebrew_install && command.check {
        let current =
            homebrew_installed_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        if let Some(latest) = homebrew_outdated_version() {
            return Ok(json!({
                "current": current,
                "latest": latest,
                "updateAvailable": true,
                "installMethod": "homebrew",
                "command": "matrix update"
            }));
        }
        return Ok(json!({
            "current": current,
            "latest": current,
            "updateAvailable": false,
            "installMethod": "homebrew",
            "command": "matrix update"
        }));
    }

    if homebrew_install && !command.check {
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

    if !command.check {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            return run_direct_linux_update(
                matrix,
                &latest,
                command.force,
                command.install_path.as_deref(),
            )
            .await;
        }
        bail!("{}", direct_update_unavailable_message(&latest));
    }

    let install_path = command
        .install_path
        .clone()
        .or_else(default_linux_install_path);
    if update_available {
        return Ok(json!({
            "current": current,
            "latest": latest,
            "updateAvailable": true,
            "installMethod": update_install_method(homebrew_install),
            "installPath": install_path,
            "command": "matrix update",
            "manualCommand": linux_manual_update_command(&latest, install_path.as_deref())
        }));
    }
    Ok(json!({
        "current": current,
        "latest": latest,
        "updateAvailable": false,
        "installMethod": update_install_method(homebrew_install),
        "installPath": install_path,
        "command": "matrix update",
        "manualCommand": linux_manual_update_command(&latest, install_path.as_deref())
    }))
}

async fn run_direct_linux_update(
    matrix: &Matrix,
    latest: &str,
    force: bool,
    install_path: Option<&Path>,
) -> Result<Value> {
    let current = env!("CARGO_PKG_VERSION");
    if !version_is_newer(current, latest) && !force {
        return Ok(Value::String(format!(
            "Already on latest version, {current}"
        )));
    }

    let release = latest_matrix_release(&matrix.client).await?;
    let asset_name = format!("matrix-{latest}-{MATRIX_LINUX_TARGET}.tar.gz");
    let asset_url = release_asset_api_url(&release, &asset_name).ok_or_else(|| {
        anyhow!("release v{latest} did not include expected Linux asset {asset_name}")
    })?;
    let archive = download_github_release_asset(&matrix.client, &asset_url).await?;
    let temp_dir = update_temp_dir(latest)?;
    fs::create_dir_all(&temp_dir)?;
    let archive_path = temp_dir.join(&asset_name);
    fs::write(&archive_path, archive)?;

    let extract_status = ProcessCommand::new("tar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&temp_dir)
        .status()
        .context("failed to run tar to extract the Linux release archive")?;
    if !extract_status.success() {
        bail!("failed to extract Linux release archive with tar -xzf");
    }

    let package_dir = temp_dir.join(asset_name.trim_end_matches(".tar.gz"));
    let extracted_matrix = package_dir.join("matrix");
    if !extracted_matrix.is_file() {
        bail!(
            "Linux release archive did not contain expected binary at {}",
            extracted_matrix.display()
        );
    }

    let current_exe = install_path
        .map(Path::to_path_buf)
        .or_else(default_linux_install_path)
        .ok_or_else(|| anyhow!("could not determine current matrix executable"))?;
    let current_exe = current_exe.canonicalize().unwrap_or(current_exe);
    let install_dir = current_exe.parent().ok_or_else(|| {
        anyhow!(
            "could not determine install directory for {}",
            current_exe.display()
        )
    })?;

    install_binary_from_archive(&extracted_matrix, &current_exe, "matrix", latest)?;
    for binary in ["matrix-enter", "matrix-construct"] {
        let installed = install_dir.join(binary);
        let extracted = package_dir.join(binary);
        if installed.exists() && extracted.is_file() {
            install_binary_from_archive(&extracted, &installed, binary, latest)?;
        }
    }
    clear_update_notice_cache();
    let _ = fs::remove_dir_all(&temp_dir);

    Ok(Value::String(format!(
        "Updated matrix from {current} to {latest}. Run `matrix --version` to confirm."
    )))
}

fn install_binary_from_archive(
    extracted: &Path,
    destination: &Path,
    binary: &str,
    latest: &str,
) -> Result<()> {
    let install_dir = destination
        .parent()
        .ok_or_else(|| anyhow!("could not determine install directory for {}", binary))?;
    let replacement = install_dir.join(format!(".{binary}-update-{}-{latest}", process::id()));
    fs::copy(extracted, &replacement).with_context(|| {
        format!(
            "could not stage updated {binary} binary in {}; try: {}",
            install_dir.display(),
            linux_manual_update_command(latest, default_linux_install_path().as_deref())
        )
    })?;
    make_executable(&replacement)?;
    fs::rename(&replacement, destination).with_context(|| {
        format!(
            "could not replace {}; try: {}",
            destination.display(),
            linux_manual_update_command(latest, default_linux_install_path().as_deref())
        )
    })?;
    Ok(())
}

fn update_install_method(homebrew_install: bool) -> &'static str {
    if homebrew_install {
        "homebrew"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "direct-linux"
    } else {
        "manual"
    }
}

fn direct_update_unavailable_message(latest: &str) -> String {
    format!(
        "this matrix install was not detected as Homebrew-managed. On macOS use `brew upgrade {MATRIX_HOMEBREW_FORMULA}`. On Linux use `{}`. From source use `{}`.",
        linux_manual_update_command(latest, default_linux_install_path().as_deref()),
        cargo_install_update_command(latest)
    )
}

fn cargo_install_update_command(latest: &str) -> String {
    format!(
        "cargo install --locked --git https://github.com/{MATRIX_REPOSITORY} --tag v{latest} matrix --force"
    )
}

fn linux_manual_update_command(latest: &str, install_path: Option<&Path>) -> String {
    let install_path = install_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "~/.cargo/bin/matrix".to_string());
    let install_command = install_path
        .strip_prefix("~/.cargo/")
        .map(|_| "install")
        .unwrap_or_else(|| {
            if install_path.starts_with("/usr/") || install_path.starts_with("/opt/") {
                "sudo install"
            } else {
                "install"
            }
        });
    format!(
        "gh release download v{latest} --repo {MATRIX_REPOSITORY} --pattern 'matrix-{latest}-{MATRIX_LINUX_TARGET}.tar.gz' --dir /tmp/matrix-update && tar -xzf /tmp/matrix-update/matrix-{latest}-{MATRIX_LINUX_TARGET}.tar.gz -C /tmp/matrix-update && {install_command} /tmp/matrix-update/matrix-{latest}-{MATRIX_LINUX_TARGET}/matrix {install_path}"
    )
}

fn default_linux_install_path() -> Option<PathBuf> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return env::current_exe()
            .ok()
            .and_then(|path| path.canonicalize().ok().or(Some(path)));
    }
    None
}

fn release_asset_api_url(release: &Value, asset_name: &str) -> Option<String> {
    release
        .get("assets")
        .and_then(Value::as_array)?
        .iter()
        .find(|asset| asset.get("name").and_then(Value::as_str) == Some(asset_name))?
        .get("url")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

async fn download_github_release_asset(
    client: &reqwest::Client,
    asset_url: &str,
) -> Result<Vec<u8>> {
    let token = github_token();
    let mut request = client
        .get(asset_url)
        .header("accept", "application/octet-stream")
        .timeout(Duration::from_secs(60));
    if let Some(token) = token.as_deref() {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        if status == StatusCode::NOT_FOUND && token.is_none() {
            bail!(
                "GitHub release asset download returned 404 for {MATRIX_REPOSITORY}; public releases should not require auth, but MATRIX_GITHUB_TOKEN or GITHUB_TOKEN can be set for authenticated GitHub API access"
            );
        }
        let text = response.text().await.unwrap_or_default();
        bail!("GitHub release asset download failed with {status}: {text}");
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .context("failed to read release asset")
}

fn update_temp_dir(latest: &str) -> Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_millis();
    Ok(env::temp_dir().join(format!("matrix-update-{latest}-{}-{now}", process::id())))
}

fn make_executable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(path, permissions)?;
    Ok(())
}

async fn maybe_print_update_notice(matrix: &Matrix, command: &Commands) {
    if matrix.output != OutputFormat::Human
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
    let body = latest_matrix_release(client).await?;
    body["tag_name"]
        .as_str()
        .map(normalize_version)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| anyhow!("latest release response did not include tag_name"))
}

async fn latest_matrix_release(client: &reqwest::Client) -> Result<Value> {
    let token = github_token();
    let mut request = client
        .get(format!(
            "https://api.github.com/repos/{MATRIX_REPOSITORY}/releases/latest"
        ))
        .timeout(Duration::from_secs(3));
    if let Some(token) = token.as_deref() {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        if status == StatusCode::NOT_FOUND && token.is_none() {
            bail!(
                "GitHub release lookup returned 404 for {MATRIX_REPOSITORY}; public releases should not require auth, but MATRIX_GITHUB_TOKEN or GITHUB_TOKEN can be set for authenticated GitHub API access"
            );
        }
        bail!(
            "GitHub release lookup failed with {status}: {}",
            error_detail(&body, "")
        );
    }
    Ok(body)
}

fn github_token() -> Option<String> {
    ["MATRIX_GITHUB_TOKEN", "GITHUB_TOKEN"]
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

async fn list_artifacts(matrix: &Matrix, args: ArtifactListArgs) -> Result<Value> {
    let mut query = Vec::new();
    push_query(&mut query, "track", args.track);
    push_query(&mut query, "subjectType", args.subject_type);
    push_query(&mut query, "subjectName", args.subject_name);
    push_query(&mut query, "subjectRepo", args.subject_repo);
    push_query(&mut query, "status", args.status);
    push_page_query(&mut query, args.page);
    let suffix = query_suffix(query);
    matrix.get(&format!("/artifacts{suffix}")).await
}

async fn list_validations(matrix: &Matrix, args: ValidationListArgs) -> Result<Value> {
    let mut query = Vec::new();
    push_query(&mut query, "track", args.track);
    push_query(&mut query, "status", args.status);
    push_query(&mut query, "scope", args.scope);
    push_page_query(&mut query, args.page);
    let suffix = query_suffix(query);
    matrix.get(&format!("/validations{suffix}")).await
}

async fn blockers(matrix: &Matrix, args: BlockersArgs) -> Result<Value> {
    let mut query = Vec::new();
    push_query(&mut query, "environment", args.environment);
    let suffix = query_suffix(query);
    matrix
        .get(&format!("/tracks/{}/blockers{suffix}", enc(&args.track)))
        .await
}

async fn sync_cache(matrix: &Matrix, args: SyncArgs) -> Result<Value> {
    let cache = sync_facts_to_cache(matrix, matrix.max_facts(args.max_facts)?)
        .await?
        .with_policy(CachePolicy::Refresh);
    Ok(json!({
        "kind": "fact-cache-sync",
        "cache": cache.to_value(),
    }))
}

async fn cache_command(matrix: &Matrix, command: CacheCommand) -> Result<Value> {
    match command.command {
        CacheSubcommand::Status => Ok(json!({
            "kind": "fact-cache-status",
            "cache": fact_cache_status(matrix)?,
        })),
        CacheSubcommand::Clear { all } => clear_fact_cache(matrix, all),
    }
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
    let context = MatrixContext::detect(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let result = execute_readonly_sql(&db, &query_text(args.sql, args.file, "SQL query")?)?;
    Ok(with_cache_summary(result, &cache))
}

#[derive(Clone, Copy)]
enum FactView {
    Deref,
    Members,
}

async fn fact_view_query(matrix: &Matrix, args: FactQueryArgs, view: FactView) -> Result<Value> {
    let context = MatrixContext::detect(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let sql = fact_view_sql(&args.fact_id, view);
    let result = execute_readonly_sql(&db, &sql)?;
    Ok(with_cache_summary(result, &cache))
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
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let sql = components_query_sql(&db, &context, options, args.limit);
    let result = execute_readonly_sql(&db, &sql)?;
    Ok(with_cache_summary(result, &cache))
}

async fn list_versions_query(matrix: &Matrix, args: VersionQueryArgs) -> Result<Value> {
    if let Some(for_component) = args.for_component.clone() {
        let component = args
            .component_filter
            .clone()
            .ok_or_else(|| anyhow!("matrix versions --for requires a component argument"))?;
        return graph_versions_for_command(
            matrix,
            GraphPairArgs {
                left: for_component,
                right: component,
                max_facts: args.max_facts,
                limit: args.limit,
                cache: args.cache.clone(),
            },
        )
        .await;
    }
    let options = args.component_options();
    let context = MatrixContext::detect_browsing(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let sql = versions_query_sql(
        &db,
        &context,
        args.component_filter.as_deref(),
        options,
        args.limit,
    );
    let result = execute_readonly_sql(&db, &sql)?;
    Ok(with_cache_summary(result, &cache))
}

async fn list_tags_query(matrix: &Matrix, args: ListQueryArgs) -> Result<Value> {
    let context = MatrixContext::detect_browsing(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let sql = tags_query_sql(&db, &context, args.limit);
    let result = execute_readonly_sql(&db, &sql)?;
    Ok(with_cache_summary(result, &cache))
}

async fn context_view_query(
    matrix: &Matrix,
    args: ListQueryArgs,
    view: ContextView,
) -> Result<Value> {
    let context = MatrixContext::detect_browsing(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let sql = context_view_sql(view, args.limit);
    let result = execute_readonly_sql(&db, &sql)?;
    Ok(with_cache_summary(result, &cache))
}

async fn compare_query(matrix: &Matrix, args: CompareArgs) -> Result<Value> {
    let context = MatrixContext::detect(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    let sql = compare_query_sql(&args.target, args.target_version.as_deref(), args.limit);
    let result = execute_readonly_sql(&db, &sql)?;
    Ok(with_cache_summary(result, &cache))
}

async fn compatible_command(matrix: &Matrix, args: CompatibleArgs) -> Result<Value> {
    match (args.left.clone(), args.right.clone()) {
        (Some(left), Some(right)) => {
            works_with_command(
                matrix,
                GraphPairArgs {
                    left,
                    right,
                    max_facts: args.max_facts,
                    limit: args.limit.clamp(1, 25),
                    cache: args.cache.clone(),
                },
            )
            .await
        }
        (None, None) => {
            context_view_query(matrix, args.list_options(), ContextView::Compatible).await
        }
        _ => bail!("matrix compatible expects both components or neither"),
    }
}

async fn graph_path_command(matrix: &Matrix, args: GraphPathArgs) -> Result<Value> {
    let (graph, cache) = load_graph(matrix, args.max_facts, &args.cache).await?;
    graph
        .path_answer(&args.source, &args.target, args.limit.max(1))
        .map(|value| with_cache_summary(value, &cache))
}

async fn works_with_command(matrix: &Matrix, args: GraphPairArgs) -> Result<Value> {
    let (graph, cache) = load_graph(matrix, args.max_facts, &args.cache).await?;
    graph
        .works_with_answer(&args.left, &args.right, args.limit.max(1))
        .map(|value| with_cache_summary(value, &cache))
}

async fn graph_why_command(matrix: &Matrix, args: GraphPairArgs) -> Result<Value> {
    let (graph, cache) = load_graph(matrix, args.max_facts, &args.cache).await?;
    let mut answer = graph.works_with_answer(&args.left, &args.right, args.limit.max(1))?;
    if let Some(object) = answer.as_object_mut() {
        object.insert("kind".to_string(), json!("graph-why"));
        object.insert(
            "hint".to_string(),
            json!("Evidence is carried on each path edge; missing paths mean Matrix lacks a connecting fact chain, not necessarily real-world incompatibility."),
        );
    }
    Ok(with_cache_summary(answer, &cache))
}

async fn producer_inventory_command(matrix: &Matrix, args: ProducerInventoryArgs) -> Result<Value> {
    let filter_context = MatrixContext {
        zone: args.context.zone.clone(),
        repo: args.context.repo.clone(),
        component: args.context.component.clone(),
        ..MatrixContext::default()
    };
    let context = MatrixContext::detect_browsing(args.context);
    let (db, cache) = query_db(matrix, args.max_facts, &context, &args.cache).await?;
    producer_inventory_value(
        &db,
        &filter_context,
        args.limit.max(1),
        args.stale_days.max(1),
    )
    .map(|value| with_cache_summary(value, &cache))
}

async fn graph_status_command(matrix: &Matrix, args: GraphStatusArgs) -> Result<Value> {
    let (graph, cache) = load_graph(matrix, args.max_facts, &args.cache).await?;
    graph
        .status_answer(&args.component, args.limit.max(1))
        .map(|value| with_cache_summary(value, &cache))
}

async fn graph_resolve_command(matrix: &Matrix, args: GraphResolveArgs) -> Result<Value> {
    let (graph, cache) = load_graph(matrix, args.max_facts, &args.cache).await?;
    graph
        .resolve_answer(&args.component)
        .map(|value| with_cache_summary(value, &cache))
}

async fn graph_versions_for_command(matrix: &Matrix, args: GraphPairArgs) -> Result<Value> {
    let (graph, cache) = load_graph(matrix, args.max_facts, &args.cache).await?;
    graph
        .versions_for_answer(&args.right, &args.left, args.limit.max(1))
        .map(|value| with_cache_summary(value, &cache))
}

async fn graph_query_command(matrix: &Matrix, args: GraphQueryArgs) -> Result<Value> {
    if args.schema {
        return Ok(json!({
            "kind": "graphql-schema",
            "schema": MATRIX_GRAPHQL_SCHEMA,
        }));
    }
    let query = query_text(args.query, args.file, "graph query")?;
    graph_query_value(
        matrix,
        &query,
        &args.vars,
        args.max_facts,
        args.limit,
        &args.cache,
    )
    .await
}

async fn graph_query_value(
    matrix: &Matrix,
    query: &str,
    vars: &[String],
    max_facts: Option<usize>,
    limit: usize,
    cache_args: &FactCacheArgs,
) -> Result<Value> {
    if is_native_graphql_query(query) {
        let max_facts = matrix.max_facts(max_facts)?;
        let options = matrix.fact_load_options(cache_args)?;
        let cached = load_query_db(matrix, max_facts, &MatrixContext::default(), options).await?;
        let graph = GraphIndex::from_db(&cached.db)?;
        let variables = parse_graphql_variables(vars)?;
        return execute_graphql_document(&cached.db, &graph, query, &variables, limit.max(1))
            .map(|value| with_cache_summary(value, &cached.cache));
    }
    let query = apply_graph_query_vars(query, vars)?;
    let (graph, cache) = load_graph(matrix, max_facts, cache_args).await?;
    graph
        .execute_request(parse_graph_query(&query)?, limit.max(1))
        .map(|value| with_cache_summary(value, &cache))
}

fn query_text(inline: Option<String>, file: Option<PathBuf>, label: &str) -> Result<String> {
    match (inline, file) {
        (Some(value), None) if !value.trim().is_empty() => Ok(value),
        (None, Some(file)) => fs::read_to_string(&file)
            .with_context(|| format!("failed to read {label} file {}", file.display()))
            .map(|value| value.trim().to_string())
            .and_then(|value| {
                if value.is_empty() {
                    bail!("{label} file was empty")
                } else {
                    Ok(value)
                }
            }),
        (Some(_), Some(_)) => bail!("provide {label} inline or with --file, not both"),
        _ => bail!("provide {label} inline or with --file"),
    }
}

async fn query_db(
    matrix: &Matrix,
    max_facts: Option<usize>,
    context: &MatrixContext,
    cache_args: &FactCacheArgs,
) -> Result<(Connection, FactCacheSummary)> {
    let max_facts = matrix.max_facts(max_facts)?;
    let options = matrix.fact_load_options(cache_args)?;
    let cached = load_query_db(matrix, max_facts, context, options).await?;
    Ok((cached.db, cached.cache))
}

async fn load_query_db(
    matrix: &Matrix,
    max_facts: usize,
    context: &MatrixContext,
    options: FactLoadOptions,
) -> Result<CachedDb> {
    let path = fact_cache_path(matrix)?;
    let source = cache_source_for_policy(matrix, &path, max_facts, options.policy).await?;
    let db = open_fact_cache_db(&path, context, matrix.sql_init()?.as_deref()).with_context(|| {
        format!(
            "no usable Matrix SQLite fact cache for this construct/profile; run `matrix sync --max-facts {max_facts}`"
        )
    })?;
    let cache = fact_cache_summary_from_db(&path, source)?.with_policy(options.policy);
    Ok(CachedDb { db, cache })
}

async fn load_graph(
    matrix: &Matrix,
    max_facts: Option<usize>,
    cache_args: &FactCacheArgs,
) -> Result<(GraphIndex, FactCacheSummary)> {
    let (db, cache) = query_db(matrix, max_facts, &MatrixContext::default(), cache_args).await?;
    Ok((GraphIndex::from_db(&db)?, cache))
}

#[derive(Clone, Debug)]
struct GraphNode {
    key: String,
    component: String,
    version: Option<String>,
    identity: Option<String>,
    subject_name: Option<String>,
    repo: Option<String>,
    status: Option<String>,
    last_observed_at: Option<String>,
}

#[derive(Clone, Debug)]
struct GraphEdge {
    source: String,
    target: String,
    relationship: String,
    capability: Option<String>,
    capability_version: Option<String>,
    source_version: Option<String>,
    target_version: Option<String>,
    source_fact_id: Option<String>,
    target_fact_id: Option<String>,
    status: Option<String>,
    observed_at: Option<String>,
}

#[derive(Clone, Debug)]
struct GraphPath {
    edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug)]
struct GraphPathScore {
    score: i64,
    confidence: &'static str,
    reasons: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct GraphIndex {
    nodes: BTreeMap<String, GraphNode>,
    aliases: HashMap<String, String>,
    alias_matches: HashMap<String, BTreeSet<String>>,
    outgoing: HashMap<String, Vec<GraphEdge>>,
    incoming: HashMap<String, Vec<GraphEdge>>,
}

#[derive(Clone, Debug)]
struct GraphRef {
    raw: String,
    name: String,
    version: Option<String>,
}

#[derive(Clone, Debug)]
enum GraphRequest {
    Path {
        source: String,
        target: String,
    },
    WorksWith {
        left: String,
        right: String,
    },
    Status {
        component: String,
    },
    VersionsFor {
        component: String,
        for_component: String,
    },
}

fn producer_inventory_value(
    db: &Connection,
    context: &MatrixContext,
    limit: usize,
    stale_days: i64,
) -> Result<Value> {
    let rows = execute_readonly_sql(db, &producer_inventory_sql(context, limit, stale_days))?;
    let producers = query_rows(&rows).as_array().cloned().unwrap_or_default();
    let total_producers = producers.len();
    let stale_producers = producers
        .iter()
        .filter(|row| {
            row.get("freshness")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "stale")
        })
        .count();
    let total_facts = producers
        .iter()
        .filter_map(|row| row.get("facts").and_then(Value::as_i64))
        .sum::<i64>();
    let invalid_facts = producers
        .iter()
        .filter_map(|row| row.get("invalid_facts").and_then(Value::as_i64))
        .sum::<i64>();
    let source_repo_facts = producers
        .iter()
        .filter_map(|row| row.get("source_repo_facts").and_then(Value::as_i64))
        .sum::<i64>();
    let inferred_subject_repo_facts = producers
        .iter()
        .filter_map(|row| {
            row.get("inferred_subject_repo_facts")
                .and_then(Value::as_i64)
        })
        .sum::<i64>();
    let unknown_producer_facts = producers
        .iter()
        .filter_map(|row| row.get("unknown_producer_facts").and_then(Value::as_i64))
        .sum::<i64>();
    Ok(json!({
        "kind": "producer-inventory",
        "summary": {
            "producers": total_producers,
            "staleProducers": stale_producers,
            "facts": total_facts,
            "invalidFacts": invalid_facts,
            "sourceRepoFacts": source_repo_facts,
            "inferredSubjectRepoFacts": inferred_subject_repo_facts,
            "unknownProducerFacts": unknown_producer_facts,
            "missingProducerMetadataFacts": inferred_subject_repo_facts + unknown_producer_facts,
            "staleAfterDays": stale_days,
        },
        "columns": rows["columns"].clone(),
        "rows": producers,
    }))
}

fn producer_inventory_sql(context: &MatrixContext, limit: usize, stale_days: i64) -> String {
    let filters = producer_context_filter_sql(context);
    format!(
        "select row_number() over (order by max(coalesce(accepted_at, observed_at)) desc, producer asc) as pick,
            producer,
            count(*) as facts,
            count(distinct component) as components,
            count(distinct zone) as zones,
            sum(case when status in ('incompatible', 'failed', 'invalid', 'blocked') then 1 else 0 end) as invalid_facts,
            sum(has_explicit_producer) as source_repo_facts,
            sum(inferred_subject_repo) as inferred_subject_repo_facts,
            sum(unknown_producer) as unknown_producer_facts,
            case
              when sum(unknown_producer) > 0 then 'unknown'
              when sum(inferred_subject_repo) > 0 and sum(has_explicit_producer) > 0 then 'mixed'
              when sum(inferred_subject_repo) > 0 then 'inferred-subject-repo'
              else 'explicit-source'
            end as producer_metadata,
            max(coalesce(accepted_at, observed_at)) as last_observed_at,
            case
              when max(coalesce(accepted_at, observed_at)) is null then 'unknown'
              when julianday('now') - julianday(max(coalesce(accepted_at, observed_at))) > {} then 'stale'
              else 'fresh'
            end as freshness
         from (
            select coalesce(nullif(source_repository, ''), nullif(source_repo, ''), nullif(repo, ''), 'unknown') as producer,
                   component, zone, status, observed_at, accepted_at,
                   case when coalesce(nullif(source_repository, ''), nullif(source_repo, '')) is not null then 1 else 0 end as has_explicit_producer,
                   case when coalesce(nullif(source_repository, ''), nullif(source_repo, '')) is null and nullif(repo, '') is not null then 1 else 0 end as inferred_subject_repo,
                   case when coalesce(nullif(source_repository, ''), nullif(source_repo, ''), nullif(repo, '')) is null then 1 else 0 end as unknown_producer
            from facts
            where 1=1 {}
         )
         group by producer
         order by last_observed_at desc, producer asc
         limit {}",
        stale_days.max(1),
        filters,
        limit.max(1)
    )
}

fn producer_context_filter_sql(context: &MatrixContext) -> String {
    let mut filters = Vec::new();
    if let Some(zone) = context.zone.as_deref() {
        filters.push(format!("facts.zone = {}", sql_literal(zone)));
    }
    if let Some(repo) = context.repo.as_deref() {
        filters.push(format!("facts.repo = {}", sql_literal(repo)));
    }
    if let Some(component) = context.component.as_deref() {
        filters.push(identity_match_sql(Some("facts"), component));
    }
    if filters.is_empty() {
        String::new()
    } else {
        format!(" and {}", filters.join(" and "))
    }
}

fn apply_graph_query_vars(query: &str, vars: &[String]) -> Result<String> {
    let mut rendered = query.to_string();
    for var in vars {
        let Some((name, value)) = var.split_once('=') else {
            bail!("graph --var expects NAME=VALUE, got {var:?}");
        };
        let name = name.trim();
        if name.is_empty() || !is_sql_identifier(name) {
            bail!("invalid graph variable name {name:?}");
        }
        let encoded = serde_json::to_string(value.trim())?;
        rendered = rendered.replace(&format!("${name}"), &encoded);
        rendered = rendered.replace(&format!("${{{name}}}"), &encoded);
    }
    Ok(rendered)
}

#[derive(Clone, Debug)]
enum GraphQlInput {
    String(String),
    Int(i64),
    Bool(bool),
    Null,
    Variable(String),
}

#[derive(Clone, Debug)]
struct GraphQlField {
    response_key: String,
    name: String,
    args: BTreeMap<String, GraphQlInput>,
    selection: Vec<GraphQlField>,
}

#[derive(Clone, Debug, PartialEq)]
enum GraphQlToken {
    Name(String),
    String(String),
    Int(i64),
    Dollar,
    Punct(char),
}

struct GraphQlParser {
    tokens: Vec<GraphQlToken>,
    index: usize,
}

fn is_native_graphql_query(query: &str) -> bool {
    let trimmed = query.trim_start();
    trimmed.starts_with('{') || trimmed.to_ascii_lowercase().starts_with("query")
}

fn parse_graphql_variables(vars: &[String]) -> Result<BTreeMap<String, Value>> {
    let mut values = BTreeMap::new();
    for var in vars {
        let Some((name, value)) = var.split_once('=') else {
            bail!("graphql --var expects NAME=VALUE, got {var:?}");
        };
        let name = name.trim();
        if name.is_empty() || !is_sql_identifier(name) {
            bail!("invalid GraphQL variable name {name:?}");
        }
        let value = value.trim();
        let parsed = serde_json::from_str::<Value>(value)
            .unwrap_or_else(|_| Value::String(value.to_string()));
        values.insert(name.to_string(), parsed);
    }
    Ok(values)
}

fn execute_graphql_document(
    db: &Connection,
    graph: &GraphIndex,
    query: &str,
    vars: &BTreeMap<String, Value>,
    default_limit: usize,
) -> Result<Value> {
    let fields = GraphQlParser::parse(query)?;
    if fields.is_empty() {
        bail!("GraphQL query must select at least one root field");
    }
    let mut data = serde_json::Map::new();
    for field in fields {
        let value = execute_graphql_field(db, graph, &field, vars, default_limit)?;
        let value = if field.selection.is_empty() {
            value
        } else {
            project_graphql_value(&value, &field.selection)
        };
        data.insert(field.response_key.clone(), value);
    }
    Ok(json!({
        "kind": "graphql-result",
        "schemaVersion": 1,
        "data": Value::Object(data),
    }))
}

fn execute_graphql_field(
    db: &Connection,
    graph: &GraphIndex,
    field: &GraphQlField,
    vars: &BTreeMap<String, Value>,
    default_limit: usize,
) -> Result<Value> {
    match field.name.as_str() {
        "path" => {
            let source = graphql_string_arg(field, vars, &["from", "source"])?
                .ok_or_else(|| anyhow!("GraphQL path requires from/source"))?;
            let target = graphql_string_arg(field, vars, &["to", "target"])?
                .ok_or_else(|| anyhow!("GraphQL path requires to/target"))?;
            let limit = graphql_usize_arg(field, vars, "limit")?.unwrap_or(default_limit);
            graph.path_answer(&source, &target, limit.max(1))
        }
        "worksWith" | "works_with" => {
            let left = graphql_string_arg(field, vars, &["left"])?
                .ok_or_else(|| anyhow!("GraphQL worksWith requires left"))?;
            let right = graphql_string_arg(field, vars, &["right"])?
                .ok_or_else(|| anyhow!("GraphQL worksWith requires right"))?;
            let limit = graphql_usize_arg(field, vars, "limit")?.unwrap_or(default_limit);
            graph.works_with_answer(&left, &right, limit.max(1))
        }
        "status" => {
            let component = graphql_string_arg(field, vars, &["component", "name"])?
                .ok_or_else(|| anyhow!("GraphQL status requires component/name"))?;
            let limit = graphql_usize_arg(field, vars, "limit")?.unwrap_or(default_limit);
            graph.status_answer(&component, limit.max(1))
        }
        "versions" => {
            let component = graphql_string_arg(field, vars, &["component"])?
                .ok_or_else(|| anyhow!("GraphQL versions requires component"))?;
            let for_component = graphql_string_arg(field, vars, &["for", "forComponent"])?
                .ok_or_else(|| anyhow!("GraphQL versions requires for/forComponent"))?;
            let limit = graphql_usize_arg(field, vars, "limit")?.unwrap_or(default_limit);
            graph.versions_for_answer(&component, &for_component, limit.max(1))
        }
        "resolve" => {
            let name = graphql_string_arg(field, vars, &["name", "component"])?
                .ok_or_else(|| anyhow!("GraphQL resolve requires name/component"))?;
            graph.resolve_answer(&name)
        }
        "producers" => {
            let limit = graphql_usize_arg(field, vars, "limit")?.unwrap_or(default_limit);
            let stale_days = graphql_i64_arg(field, vars, &["staleDays", "stale_days"])?
                .unwrap_or(14)
                .max(1);
            producer_inventory_value(db, &MatrixContext::default(), limit.max(1), stale_days)
        }
        other => bail!(
            "unsupported GraphQL root field {other:?}; expected path, worksWith, status, versions, resolve, or producers"
        ),
    }
}

fn graphql_string_arg(
    field: &GraphQlField,
    vars: &BTreeMap<String, Value>,
    names: &[&str],
) -> Result<Option<String>> {
    for name in names {
        if let Some(value) = field.args.get(*name) {
            return graphql_input_string(value, vars).map(Some);
        }
    }
    Ok(None)
}

fn graphql_usize_arg(
    field: &GraphQlField,
    vars: &BTreeMap<String, Value>,
    name: &str,
) -> Result<Option<usize>> {
    Ok(graphql_i64_arg(field, vars, &[name])?.map(|value| value.max(1) as usize))
}

fn graphql_i64_arg(
    field: &GraphQlField,
    vars: &BTreeMap<String, Value>,
    names: &[&str],
) -> Result<Option<i64>> {
    for name in names {
        if let Some(value) = field.args.get(*name) {
            return graphql_input_i64(value, vars).map(Some);
        }
    }
    Ok(None)
}

fn graphql_input_string(input: &GraphQlInput, vars: &BTreeMap<String, Value>) -> Result<String> {
    match resolve_graphql_input(input, vars)? {
        Value::String(value) => Ok(value),
        other => bail!(
            "GraphQL argument expected String, got {}",
            human_inline_value(&other)
        ),
    }
}

fn graphql_input_i64(input: &GraphQlInput, vars: &BTreeMap<String, Value>) -> Result<i64> {
    match resolve_graphql_input(input, vars)? {
        Value::Number(value) => value
            .as_i64()
            .ok_or_else(|| anyhow!("GraphQL argument expected integer")),
        Value::String(value) => value
            .parse::<i64>()
            .with_context(|| format!("GraphQL argument expected integer, got {value:?}")),
        other => bail!(
            "GraphQL argument expected Int, got {}",
            human_inline_value(&other)
        ),
    }
}

fn resolve_graphql_input(input: &GraphQlInput, vars: &BTreeMap<String, Value>) -> Result<Value> {
    match input {
        GraphQlInput::String(value) => Ok(Value::String(value.clone())),
        GraphQlInput::Int(value) => Ok(json!(value)),
        GraphQlInput::Bool(value) => Ok(json!(value)),
        GraphQlInput::Null => Ok(Value::Null),
        GraphQlInput::Variable(name) => vars
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("missing GraphQL variable ${name}")),
    }
}

fn project_graphql_value(value: &Value, selection: &[GraphQlField]) -> Value {
    match value {
        Value::Object(object) => {
            let mut projected = serde_json::Map::new();
            for field in selection {
                let child = object.get(&field.name).cloned().unwrap_or(Value::Null);
                let child = if field.selection.is_empty() {
                    child
                } else {
                    project_graphql_value(&child, &field.selection)
                };
                projected.insert(field.response_key.clone(), child);
            }
            Value::Object(projected)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| project_graphql_value(value, selection))
                .collect(),
        ),
        _ => value.clone(),
    }
}

impl GraphQlParser {
    fn parse(query: &str) -> Result<Vec<GraphQlField>> {
        let mut parser = Self {
            tokens: tokenize_graphql(query)?,
            index: 0,
        };
        parser.parse_document()
    }

    fn parse_document(&mut self) -> Result<Vec<GraphQlField>> {
        if self.consume_name("query") {
            if matches!(self.peek(), Some(GraphQlToken::Name(_))) {
                self.index += 1;
            }
            if matches!(self.peek(), Some(GraphQlToken::Punct('('))) {
                self.skip_balanced('(', ')')?;
            }
        } else if self.peek_name("mutation") || self.peek_name("subscription") {
            bail!("Matrix GraphQL only supports query operations");
        }
        let fields = self.parse_selection_set()?;
        if self.peek().is_some() {
            bail!("unexpected token after GraphQL query");
        }
        Ok(fields)
    }

    fn parse_selection_set(&mut self) -> Result<Vec<GraphQlField>> {
        self.expect_punct('{')?;
        let mut fields = Vec::new();
        while !self.consume_punct('}') {
            if self.peek().is_none() {
                bail!("unterminated GraphQL selection set");
            }
            fields.push(self.parse_field()?);
        }
        Ok(fields)
    }

    fn parse_field(&mut self) -> Result<GraphQlField> {
        let first = self.expect_name()?;
        let (response_key, name) = if self.consume_punct(':') {
            (first, self.expect_name()?)
        } else {
            (first.clone(), first)
        };
        let args = if self.consume_punct('(') {
            self.parse_args()?
        } else {
            BTreeMap::new()
        };
        let selection = if matches!(self.peek(), Some(GraphQlToken::Punct('{'))) {
            self.parse_selection_set()?
        } else {
            Vec::new()
        };
        Ok(GraphQlField {
            response_key,
            name,
            args,
            selection,
        })
    }

    fn parse_args(&mut self) -> Result<BTreeMap<String, GraphQlInput>> {
        let mut args = BTreeMap::new();
        while !self.consume_punct(')') {
            if self.peek().is_none() {
                bail!("unterminated GraphQL argument list");
            }
            let name = self.expect_name()?;
            self.expect_punct(':')?;
            let value = self.parse_input()?;
            args.insert(name, value);
        }
        Ok(args)
    }

    fn parse_input(&mut self) -> Result<GraphQlInput> {
        match self.next() {
            Some(GraphQlToken::String(value)) => Ok(GraphQlInput::String(value)),
            Some(GraphQlToken::Int(value)) => Ok(GraphQlInput::Int(value)),
            Some(GraphQlToken::Dollar) => Ok(GraphQlInput::Variable(self.expect_name()?)),
            Some(GraphQlToken::Name(value)) if value == "true" => Ok(GraphQlInput::Bool(true)),
            Some(GraphQlToken::Name(value)) if value == "false" => Ok(GraphQlInput::Bool(false)),
            Some(GraphQlToken::Name(value)) if value == "null" => Ok(GraphQlInput::Null),
            Some(GraphQlToken::Name(value)) => Ok(GraphQlInput::String(value)),
            other => bail!("expected GraphQL value, got {other:?}"),
        }
    }

    fn skip_balanced(&mut self, open: char, close: char) -> Result<()> {
        self.expect_punct(open)?;
        let mut depth = 1usize;
        while let Some(token) = self.next() {
            match token {
                GraphQlToken::Punct(value) if value == open => depth += 1,
                GraphQlToken::Punct(value) if value == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        bail!("unterminated GraphQL variable definition list")
    }

    fn expect_name(&mut self) -> Result<String> {
        match self.next() {
            Some(GraphQlToken::Name(value)) => Ok(value),
            other => bail!("expected GraphQL name, got {other:?}"),
        }
    }

    fn expect_punct(&mut self, expected: char) -> Result<()> {
        if self.consume_punct(expected) {
            Ok(())
        } else {
            bail!("expected GraphQL punctuation {expected:?}")
        }
    }

    fn consume_name(&mut self, expected: &str) -> bool {
        if self.peek_name(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn consume_punct(&mut self, expected: char) -> bool {
        if matches!(self.peek(), Some(GraphQlToken::Punct(value)) if *value == expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek_name(&self, expected: &str) -> bool {
        matches!(self.peek(), Some(GraphQlToken::Name(value)) if value == expected)
    }

    fn peek(&self) -> Option<&GraphQlToken> {
        self.tokens.get(self.index)
    }

    fn next(&mut self) -> Option<GraphQlToken> {
        let token = self.tokens.get(self.index).cloned();
        if token.is_some() {
            self.index += 1;
        }
        token
    }
}

fn tokenize_graphql(query: &str) -> Result<Vec<GraphQlToken>> {
    let mut tokens = Vec::new();
    let mut chars = query.char_indices().peekable();
    while let Some((_, character)) = chars.next() {
        match character {
            character if character.is_whitespace() || character == ',' => {}
            '#' => {
                for (_, next) in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '{' | '}' | '(' | ')' | ':' | '!' | '[' | ']' | '=' => {
                tokens.push(GraphQlToken::Punct(character));
            }
            '$' => tokens.push(GraphQlToken::Dollar),
            '"' => tokens.push(GraphQlToken::String(read_graphql_string(&mut chars)?)),
            '-' | '0'..='9' => {
                let mut text = character.to_string();
                while let Some((_, next)) = chars.peek() {
                    if next.is_ascii_digit() {
                        text.push(*next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens
                    .push(GraphQlToken::Int(text.parse::<i64>().with_context(
                        || format!("invalid GraphQL integer {text:?}"),
                    )?));
            }
            character if is_graphql_name_start(character) => {
                let mut text = character.to_string();
                while let Some((_, next)) = chars.peek() {
                    if is_graphql_name_continue(*next) {
                        text.push(*next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(GraphQlToken::Name(text));
            }
            other => bail!("unexpected GraphQL character {other:?}"),
        }
    }
    Ok(tokens)
}

fn read_graphql_string<I>(chars: &mut std::iter::Peekable<I>) -> Result<String>
where
    I: Iterator<Item = (usize, char)>,
{
    let mut value = String::new();
    while let Some((_, character)) = chars.next() {
        match character {
            '"' => return Ok(value),
            '\\' => {
                let Some((_, escaped)) = chars.next() else {
                    bail!("unterminated GraphQL string escape");
                };
                value.push(match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
            }
            other => value.push(other),
        }
    }
    bail!("unterminated GraphQL string")
}

fn is_graphql_name_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

fn is_graphql_name_continue(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

impl GraphIndex {
    fn from_db(db: &Connection) -> Result<Self> {
        let mut graph = Self::default();
        graph.load_nodes(db)?;
        graph.load_edges(db)?;
        Ok(graph)
    }

    fn execute_request(&self, request: GraphRequest, limit: usize) -> Result<Value> {
        match request {
            GraphRequest::Path { source, target } => self.path_answer(&source, &target, limit),
            GraphRequest::WorksWith { left, right } => self.works_with_answer(&left, &right, limit),
            GraphRequest::Status { component } => self.status_answer(&component, limit),
            GraphRequest::VersionsFor {
                component,
                for_component,
            } => self.versions_for_answer(&component, &for_component, limit),
        }
    }

    fn load_nodes(&mut self, db: &Connection) -> Result<()> {
        let mut stmt = db.prepare(
            "select component, canonical_component, identity, subject_name, repo,
                    version, status, max(observed_at) as last_observed_at
             from components
             where component is not null and component != ''
             group by component, canonical_component, identity, subject_name, repo, version, status
             order by last_observed_at desc",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(GraphNode {
                key: graph_key(&row.get::<_, String>(0)?),
                component: row.get::<_, String>(0)?,
                version: row.get::<_, Option<String>>(5)?,
                identity: row.get::<_, Option<String>>(2)?,
                subject_name: row.get::<_, Option<String>>(3)?,
                repo: row.get::<_, Option<String>>(4)?,
                status: row.get::<_, Option<String>>(6)?,
                last_observed_at: row.get::<_, Option<String>>(7)?,
            })
        })?;
        for node in rows {
            self.add_node(node?);
        }

        let mut stmt = db.prepare(
            "select identity, canonical_component, subject_name, repo, alias
             from identity_aliases
             where alias is not null and alias != ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (identity, canonical_component, subject_name, repo, alias) = row?;
            let key = canonical_component
                .or_else(|| subject_name.clone().map(|value| component_key(&value)))
                .or_else(|| repo.clone().map(|value| component_key(&value)))
                .or_else(|| identity.clone().map(|value| component_key(&value)));
            if let Some(key) = key {
                self.add_alias(&alias, &graph_key(&key));
            }
        }
        Ok(())
    }

    fn add_node(&mut self, node: GraphNode) {
        let key = node.key.clone();
        let replace = self
            .nodes
            .get(&key)
            .map(|existing| {
                node.last_observed_at > existing.last_observed_at
                    || (existing.version.is_none() && node.version.is_some())
            })
            .unwrap_or(true);
        self.add_alias(&node.component, &key);
        if let Some(value) = node.subject_name.clone() {
            self.add_alias(&value, &key);
        }
        if let Some(value) = node.repo.clone() {
            self.add_alias(&value, &key);
        }
        if let Some(value) = node.identity.clone() {
            self.add_alias(&value, &key);
        }
        if replace {
            self.nodes.insert(key, node);
        }
    }

    fn add_alias(&mut self, alias: &str, key: &str) {
        for value in alias_variants(alias) {
            self.alias_matches
                .entry(value.clone())
                .or_default()
                .insert(key.to_string());
            self.aliases.entry(value).or_insert_with(|| key.to_string());
        }
    }

    fn load_edges(&mut self, db: &Connection) -> Result<()> {
        let sql = "
            select r.component as source_component, p.component as target_component,
                   'requires' as relationship, r.capability, r.capability_version,
                   r.version as source_version, p.version as target_version,
                   r.fact_id as source_fact_id, p.fact_id as target_fact_id,
                   coalesce(p.status, r.status) as status,
                   null as observed_at
            from requirements r
            join capabilities p
              on p.capability = r.capability
             and (r.capability_version is null or p.capability_version = r.capability_version)
            where r.component is not null and p.component is not null
            union all
            select fact_component as source_component, component as target_component,
                   'contains' as relationship, null as capability, null as capability_version,
                   fact_version as source_version, version as target_version,
                   fact_id as source_fact_id, fact_id as target_fact_id,
                   fact_status as status, null as observed_at
            from members
            where fact_component is not null and component is not null";
        let mut stmt = db.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(GraphEdge {
                source: graph_key(&row.get::<_, String>(0)?),
                target: graph_key(&row.get::<_, String>(1)?),
                relationship: row.get::<_, String>(2)?,
                capability: row.get::<_, Option<String>>(3)?,
                capability_version: row.get::<_, Option<String>>(4)?,
                source_version: row.get::<_, Option<String>>(5)?,
                target_version: row.get::<_, Option<String>>(6)?,
                source_fact_id: row.get::<_, Option<String>>(7)?,
                target_fact_id: row.get::<_, Option<String>>(8)?,
                status: row.get::<_, Option<String>>(9)?,
                observed_at: row.get::<_, Option<String>>(10)?,
            })
        })?;
        let mut seen = BTreeSet::new();
        for row in rows {
            let edge = row?;
            let reverse_member = if edge.relationship == "contains" {
                Some(GraphEdge {
                    source: edge.target.clone(),
                    target: edge.source.clone(),
                    relationship: "member-of".to_string(),
                    capability: edge.capability.clone(),
                    capability_version: edge.capability_version.clone(),
                    source_version: edge.target_version.clone(),
                    target_version: edge.source_version.clone(),
                    source_fact_id: edge.target_fact_id.clone(),
                    target_fact_id: edge.source_fact_id.clone(),
                    status: edge.status.clone(),
                    observed_at: edge.observed_at.clone(),
                })
            } else {
                None
            };
            self.add_edge(edge, &mut seen);
            if let Some(edge) = reverse_member {
                self.add_edge(edge, &mut seen);
            }
        }
        Ok(())
    }

    fn add_edge(&mut self, edge: GraphEdge, seen: &mut BTreeSet<String>) {
        let dedupe_key = format!(
            "{}\0{}\0{}\0{}\0{}",
            edge.source,
            edge.target,
            edge.relationship,
            edge.capability.clone().unwrap_or_default(),
            edge.capability_version.clone().unwrap_or_default()
        );
        if seen.insert(dedupe_key) {
            self.outgoing
                .entry(edge.source.clone())
                .or_default()
                .push(edge.clone());
            self.incoming
                .entry(edge.target.clone())
                .or_default()
                .push(edge);
        }
    }

    fn resolve(&self, value: &str) -> GraphRef {
        let parsed = parse_graph_ref(value);
        let key = self
            .aliases
            .get(&graph_alias_key(&parsed.name))
            .cloned()
            .unwrap_or_else(|| graph_key(&parsed.name));
        GraphRef {
            raw: parsed.raw,
            name: key,
            version: parsed.version,
        }
    }

    fn resolve_answer(&self, value: &str) -> Result<Value> {
        let parsed = parse_graph_ref(value);
        let alias_key = graph_alias_key(&parsed.name);
        let resolved = self.resolve(value);
        let matches = self
            .alias_matches
            .get(&alias_key)
            .cloned()
            .unwrap_or_else(|| BTreeSet::from([resolved.name.clone()]))
            .into_iter()
            .map(|key| {
                json!({
                    "node": self.node_value(&key, value),
                    "aliasKinds": self.alias_kinds_for(&key, &parsed.name),
                    "outgoingCount": self.outgoing.get(&key).map(Vec::len).unwrap_or(0),
                    "incomingCount": self.incoming.get(&key).map(Vec::len).unwrap_or(0),
                })
            })
            .collect::<Vec<_>>();
        let ambiguous = matches.len() > 1;
        Ok(json!({
            "kind": "graph-resolve",
            "requested": value,
            "name": parsed.name,
            "version": parsed.version,
            "resolved": self.node_value(&resolved.name, value),
            "ambiguous": ambiguous,
            "matchCount": matches.len(),
            "matches": matches,
            "warnings": self.resolution_warnings(value, &resolved.name, ambiguous),
        }))
    }

    fn alias_kinds_for(&self, key: &str, requested: &str) -> Vec<String> {
        let Some(node) = self.nodes.get(key) else {
            return Vec::new();
        };
        let requested = graph_alias_key(requested);
        let mut kinds = Vec::new();
        if graph_alias_key(&node.component) == requested {
            kinds.push("component".to_string());
        }
        if node
            .subject_name
            .as_deref()
            .is_some_and(|value| graph_alias_key(value) == requested)
        {
            kinds.push("subject-name".to_string());
        }
        if node
            .repo
            .as_deref()
            .is_some_and(|value| graph_alias_key(value) == requested)
        {
            kinds.push("repo".to_string());
        }
        if node
            .identity
            .as_deref()
            .is_some_and(|value| graph_alias_key(value) == requested)
        {
            kinds.push("identity".to_string());
        }
        if kinds.is_empty() && graph_key(requested.as_str()) == node.key {
            kinds.push("component-key".to_string());
        }
        kinds
    }

    fn resolution_warnings(
        &self,
        requested: &str,
        resolved_key: &str,
        ambiguous: bool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        if ambiguous {
            warnings.push("multiple graph nodes match this name; use a more specific repo, package, identity, or component".to_string());
        }
        if let Some(node) = self.nodes.get(resolved_key) {
            let kinds = self.alias_kinds_for(resolved_key, requested);
            if graph_alias_key(requested) != graph_alias_key(&node.component)
                || !kinds.iter().any(|kind| kind == "component")
            {
                let through = if kinds.is_empty() {
                    "alias".to_string()
                } else {
                    kinds.join(", ")
                };
                warnings.push(format!(
                    "{requested:?} resolved to component {:?} through {through}",
                    node.component
                ));
            }
        }
        warnings
    }

    fn path_answer(&self, source: &str, target: &str, limit: usize) -> Result<Value> {
        let source_ref = self.resolve(source);
        let target_ref = self.resolve(target);
        let paths = self.find_paths(&source_ref.name, &target_ref.name, limit);
        let recommended = paths.first().map(|path| self.path_value(path));
        let confidence = recommended
            .as_ref()
            .and_then(|path| path.get("confidence"))
            .cloned()
            .unwrap_or_else(|| json!("unknown"));
        Ok(json!({
            "kind": "graph-path",
            "source": self.node_value(&source_ref.name, source),
            "target": self.node_value(&target_ref.name, target),
            "status": if paths.is_empty() { "unknown" } else { "connected" },
            "found": !paths.is_empty(),
            "confidence": confidence,
            "recommended": recommended,
            "pathCount": paths.len(),
            "paths": paths.iter().map(|path| self.path_value(path)).collect::<Vec<_>>(),
            "missing": if paths.is_empty() { json!(["no known fact path connects these components"]) } else { json!([]) },
        }))
    }

    fn works_with_answer(&self, left: &str, right: &str, limit: usize) -> Result<Value> {
        let left_ref = self.resolve(left);
        let right_ref = self.resolve(right);
        let (direction, paths) = self.best_directional_path(&left_ref.name, &right_ref.name, limit);
        let compatible = paths
            .iter()
            .flat_map(|path| path.edges.iter())
            .all(|edge| !is_invalid_status(edge.status.as_deref()));
        let recommended = paths.first().map(|path| self.path_value(path));
        let confidence = recommended
            .as_ref()
            .and_then(|path| path.get("confidence"))
            .cloned()
            .unwrap_or_else(|| json!("unknown"));
        let reasons = recommended
            .as_ref()
            .and_then(|path| path.get("reasons"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        Ok(json!({
            "kind": "graph-works-with",
            "left": self.node_value(&left_ref.name, left),
            "right": self.node_value(&right_ref.name, right),
            "status": if paths.is_empty() {
                "unknown"
            } else if compatible {
                "compatible"
            } else {
                "blocked"
            },
            "compatible": !paths.is_empty() && compatible,
            "confidence": confidence,
            "reasons": reasons,
            "recommended": recommended,
            "direction": direction,
            "pathCount": paths.len(),
            "paths": paths.iter().map(|path| self.path_value(path)).collect::<Vec<_>>(),
            "missing": if paths.is_empty() { json!(["no known compatibility path connects these components"]) } else { json!([]) },
        }))
    }

    fn status_answer(&self, component: &str, limit: usize) -> Result<Value> {
        let component_ref = self.resolve(component);
        let outgoing = self
            .outgoing
            .get(&component_ref.name)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .map(|edge| self.edge_value(&edge))
            .collect::<Vec<_>>();
        let incoming = self
            .incoming
            .get(&component_ref.name)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .map(|edge| self.edge_value(&edge))
            .collect::<Vec<_>>();
        Ok(json!({
            "kind": "graph-status",
            "component": self.node_value(&component_ref.name, component),
            "outgoing": outgoing,
            "incoming": incoming,
            "outgoingCount": self.outgoing.get(&component_ref.name).map(Vec::len).unwrap_or(0),
            "incomingCount": self.incoming.get(&component_ref.name).map(Vec::len).unwrap_or(0),
        }))
    }

    fn versions_for_answer(
        &self,
        component: &str,
        for_component: &str,
        limit: usize,
    ) -> Result<Value> {
        let component_ref = self.resolve(component);
        let for_ref = self.resolve(for_component);
        let mut versions: BTreeMap<String, (i64, String, usize)> = BTreeMap::new();
        for path in self
            .find_paths(&for_ref.name, &component_ref.name, limit)
            .into_iter()
            .chain(self.find_paths(&component_ref.name, &for_ref.name, limit))
        {
            let score = self.score_path(&path);
            for edge in path.edges {
                if edge.source == component_ref.name
                    && let Some(version) = edge.source_version
                {
                    record_version_candidate(&mut versions, version, &score);
                }
                if edge.target == component_ref.name
                    && let Some(version) = edge.target_version
                {
                    record_version_candidate(&mut versions, version, &score);
                }
            }
        }
        if versions.is_empty()
            && let Some(node) = self.nodes.get(&component_ref.name)
            && let Some(version) = node.version.clone()
        {
            versions.insert(version, (25, "low".to_string(), 1));
        }
        let mut candidates = versions
            .into_iter()
            .map(|(version, (score, confidence, path_count))| {
                json!({
                    "version": version,
                    "score": score,
                    "confidence": confidence,
                    "pathCount": path_count,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let right_score = right.get("score").and_then(Value::as_i64).unwrap_or(0);
            let left_score = left.get("score").and_then(Value::as_i64).unwrap_or(0);
            right_score.cmp(&left_score).then_with(|| {
                right
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .cmp(
                        left.get("version")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
            })
        });
        let versions = candidates
            .iter()
            .take(limit)
            .filter_map(|candidate| candidate.get("version").cloned())
            .collect::<Vec<_>>();
        Ok(json!({
            "kind": "graph-versions-for",
            "component": self.node_value(&component_ref.name, component),
            "for": self.node_value(&for_ref.name, for_component),
            "versions": versions,
            "versionCandidates": candidates.into_iter().take(limit).collect::<Vec<_>>(),
        }))
    }

    fn best_directional_path(
        &self,
        left: &str,
        right: &str,
        limit: usize,
    ) -> (&'static str, Vec<GraphPath>) {
        let forward = self.find_paths(left, right, limit);
        let reverse = self.find_paths(right, left, limit);
        match (forward.first(), reverse.first()) {
            (Some(left_path), Some(right_path)) => {
                let left_score = self.score_path(left_path).score;
                let right_score = self.score_path(right_path).score;
                if left_score >= right_score {
                    ("left_to_right", forward)
                } else {
                    ("right_to_left", reverse)
                }
            }
            (Some(_), None) => ("left_to_right", forward),
            (None, Some(_)) => ("right_to_left", reverse),
            (None, None) => ("none", Vec::new()),
        }
    }

    fn find_paths(&self, source: &str, target: &str, limit: usize) -> Vec<GraphPath> {
        if source == target {
            return vec![GraphPath { edges: Vec::new() }];
        }
        const MAX_DEPTH: usize = 6;
        let search_limit = limit.saturating_mul(8).clamp(limit.max(1), 80);
        let mut paths = Vec::new();
        let mut queue = VecDeque::from([(source.to_string(), Vec::<GraphEdge>::new())]);
        while let Some((current, path)) = queue.pop_front() {
            if path.len() >= MAX_DEPTH {
                continue;
            }
            for edge in self.outgoing.get(&current).cloned().unwrap_or_default() {
                if path.iter().any(|seen| seen.source == edge.target) || edge.target == source {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push(edge.clone());
                if edge.target == target {
                    paths.push(GraphPath { edges: next_path });
                    if paths.len() >= search_limit {
                        return self.rank_paths(paths, limit);
                    }
                } else {
                    queue.push_back((edge.target.clone(), next_path));
                }
            }
        }
        self.rank_paths(paths, limit)
    }

    fn rank_paths(&self, mut paths: Vec<GraphPath>, limit: usize) -> Vec<GraphPath> {
        paths.sort_by(|left, right| {
            let left_score = self.score_path(left).score;
            let right_score = self.score_path(right).score;
            right_score
                .cmp(&left_score)
                .then_with(|| left.edges.len().cmp(&right.edges.len()))
                .then_with(|| {
                    left.node_keys()
                        .join("\0")
                        .cmp(&right.node_keys().join("\0"))
                })
        });
        paths.into_iter().take(limit.max(1)).collect()
    }

    fn score_path(&self, path: &GraphPath) -> GraphPathScore {
        if path.edges.is_empty() {
            return GraphPathScore {
                score: 100,
                confidence: "high",
                reasons: vec!["same component".to_string()],
            };
        }
        let mut score = 100 - (path.edges.len() as i64 * 8);
        let mut reasons = Vec::new();
        if path.edges.len() == 1 {
            score += 10;
            reasons.push("direct evidence".to_string());
        } else {
            reasons.push("inferred multi-hop path".to_string());
        }
        if path
            .edges
            .iter()
            .any(|edge| is_invalid_status(edge.status.as_deref()))
        {
            score -= 70;
            reasons.push("contains blocked or failed evidence".to_string());
        } else if path
            .edges
            .iter()
            .all(|edge| is_positive_status(edge.status.as_deref()))
        {
            score += 20;
            reasons.push("all edge statuses are passing".to_string());
        }
        if path.edges.iter().all(|edge| {
            edge.source_fact_id.is_some()
                && edge.target_fact_id.is_some()
                && (edge.source_version.is_some() || edge.target_version.is_some())
        }) {
            score += 15;
            reasons.push("versioned fact evidence".to_string());
        } else {
            score -= 10;
            reasons.push("some version or fact details are missing".to_string());
        }
        let confidence = if path
            .edges
            .iter()
            .any(|edge| is_invalid_status(edge.status.as_deref()))
        {
            "blocked"
        } else if score >= 120 {
            "high"
        } else if score >= 75 {
            "medium"
        } else {
            "low"
        };
        GraphPathScore {
            score,
            confidence,
            reasons,
        }
    }

    fn path_value(&self, path: &GraphPath) -> Value {
        let score = self.score_path(path);
        let nodes = path
            .node_keys()
            .into_iter()
            .map(|key| self.node_value(&key, &key))
            .collect::<Vec<_>>();
        json!({
            "length": path.edges.len(),
            "score": score.score,
            "confidence": score.confidence,
            "reasons": score.reasons,
            "nodes": nodes,
            "edges": path.edges.iter().map(|edge| self.edge_value(edge)).collect::<Vec<_>>(),
        })
    }

    fn node_value(&self, key: &str, requested: &str) -> Value {
        if let Some(node) = self.nodes.get(key) {
            json!({
                "requested": requested,
                "key": node.key,
                "component": node.component,
                "version": node.version,
                "identity": node.identity,
                "subjectName": node.subject_name,
                "repo": node.repo,
                "status": node.status,
                "lastObservedAt": node.last_observed_at,
            })
        } else {
            json!({
                "requested": requested,
                "key": key,
                "component": key,
                "version": null,
                "status": "unknown",
            })
        }
    }

    fn edge_value(&self, edge: &GraphEdge) -> Value {
        json!({
            "from": self.node_value(&edge.source, &edge.source),
            "to": self.node_value(&edge.target, &edge.target),
            "relationship": edge.relationship,
            "capability": edge.capability,
            "capabilityVersion": edge.capability_version,
            "sourceVersion": edge.source_version,
            "targetVersion": edge.target_version,
            "sourceFactId": edge.source_fact_id,
            "targetFactId": edge.target_fact_id,
            "status": edge.status,
            "observedAt": edge.observed_at,
        })
    }
}

impl GraphPath {
    fn node_keys(&self) -> Vec<String> {
        let mut nodes = Vec::new();
        if let Some(first) = self.edges.first() {
            nodes.push(first.source.clone());
        }
        for edge in &self.edges {
            nodes.push(edge.target.clone());
        }
        nodes
    }
}

fn parse_graph_ref(value: &str) -> GraphRef {
    let raw = value.trim().to_string();
    let (name, version) = split_graph_version(&raw);
    GraphRef { raw, name, version }
}

fn split_graph_version(value: &str) -> (String, Option<String>) {
    if let Some(index) = value.rfind('@')
        && index > 0
        && !value[index + 1..].contains('/')
    {
        return (
            value[..index].to_string(),
            Some(value[index + 1..].to_string()),
        );
    }
    (value.to_string(), None)
}

fn graph_key(value: &str) -> String {
    component_key(value).to_ascii_lowercase()
}

fn graph_alias_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn alias_variants(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    let mut variants = BTreeSet::new();
    if !trimmed.is_empty() {
        variants.insert(trimmed.to_ascii_lowercase());
        variants.insert(component_key(trimmed).to_ascii_lowercase());
    }
    variants.into_iter().collect()
}

fn is_invalid_status(status: Option<&str>) -> bool {
    matches!(
        status.map(|value| value.to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "incompatible" | "failed" | "invalid" | "blocked")
    )
}

fn is_positive_status(status: Option<&str>) -> bool {
    matches!(
        status.map(|value| value.to_ascii_lowercase()),
        Some(value)
            if matches!(
                value.as_str(),
                "compatible" | "passed" | "observed" | "candidate" | "valid" | "ready" | "success" | "succeeded"
            )
    )
}

fn record_version_candidate(
    versions: &mut BTreeMap<String, (i64, String, usize)>,
    version: String,
    score: &GraphPathScore,
) {
    versions
        .entry(version)
        .and_modify(|existing| {
            existing.0 = existing.0.max(score.score);
            existing.1 = strongest_confidence(&existing.1, score.confidence).to_string();
            existing.2 += 1;
        })
        .or_insert((score.score, score.confidence.to_string(), 1));
}

fn strongest_confidence(left: &str, right: &str) -> &'static str {
    let rank = |value: &str| match value {
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "blocked" => 1,
        _ => 0,
    };
    let stronger = if rank(left) >= rank(right) {
        left
    } else {
        right
    };
    match stronger {
        "high" => "high",
        "medium" => "medium",
        "low" => "low",
        "blocked" => "blocked",
        _ => "unknown",
    }
}

fn parse_graph_query(query: &str) -> Result<GraphRequest> {
    let trimmed = normalize_graph_query(query);
    if let Some((source, target)) = trimmed.split_once("->") {
        return Ok(GraphRequest::Path {
            source: source.trim().trim_matches('"').to_string(),
            target: target.trim().trim_matches('"').to_string(),
        });
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("path") {
        return Ok(GraphRequest::Path {
            source: graph_query_arg(trimmed, "from")
                .or_else(|| graph_query_arg(trimmed, "source"))
                .ok_or_else(|| anyhow!("path query requires from/source"))?,
            target: graph_query_arg(trimmed, "to")
                .or_else(|| graph_query_arg(trimmed, "target"))
                .ok_or_else(|| anyhow!("path query requires to/target"))?,
        });
    }
    if lower.starts_with("workswith") || lower.starts_with("works_with") {
        return Ok(GraphRequest::WorksWith {
            left: graph_query_arg(trimmed, "left")
                .ok_or_else(|| anyhow!("worksWith query requires left"))?,
            right: graph_query_arg(trimmed, "right")
                .ok_or_else(|| anyhow!("worksWith query requires right"))?,
        });
    }
    if lower.starts_with("status") {
        return Ok(GraphRequest::Status {
            component: graph_query_arg(trimmed, "component")
                .or_else(|| graph_query_arg(trimmed, "name"))
                .ok_or_else(|| anyhow!("status query requires component/name"))?,
        });
    }
    if lower.starts_with("versions") {
        return Ok(GraphRequest::VersionsFor {
            component: graph_query_arg(trimmed, "component")
                .ok_or_else(|| anyhow!("versions query requires component"))?,
            for_component: graph_query_arg(trimmed, "for")
                .or_else(|| graph_query_arg(trimmed, "forComponent"))
                .ok_or_else(|| anyhow!("versions query requires for/forComponent"))?,
        });
    }
    bail!(
        "unsupported graph query; use `a -> b`, `path(from:\"a\", to:\"b\")`, `worksWith(left:\"a\", right:\"b\")`, `status(component:\"a\")`, or `versions(component:\"a\", for:\"b\")`"
    )
}

fn normalize_graph_query(query: &str) -> &str {
    let trimmed = query.trim();
    if trimmed.contains("->") {
        return trimmed;
    }
    let lower = trimmed.to_ascii_lowercase();
    for field in ["path", "workswith", "works_with", "status", "versions"] {
        if let Some(index) = lower.find(field) {
            let before = lower[..index].chars().next_back();
            let after = lower[index + field.len()..].chars().next();
            let starts_field = before
                .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'));
            let ends_field = after
                .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'))
                || after == Some('(');
            if starts_field && ends_field {
                return trimmed[index..].trim_start();
            }
        }
    }
    trimmed
}

fn graph_query_arg(query: &str, name: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    let needle = format!("{}:", name.to_ascii_lowercase());
    let start = lower.find(&needle)? + needle.len();
    let rest = query[start..].trim_start();
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    if let Some(rest) = rest.strip_prefix('\'') {
        let end = rest.find('\'')?;
        return Some(rest[..end].to_string());
    }
    let end = rest.find([',', ')', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_string()).filter(|value| !value.is_empty())
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

impl CompatibleArgs {
    fn list_options(&self) -> ListQueryArgs {
        ListQueryArgs {
            max_facts: self.max_facts,
            limit: self.limit,
            all: self.all,
            type_filter: self.type_filter.clone(),
            include_applications: self.include_applications,
            include_dependencies: self.include_dependencies,
            cache: self.cache.clone(),
            context: self.context.clone(),
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

#[cfg(feature = "interactive")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplGraphArgs {
    query: Option<String>,
    file: Option<PathBuf>,
    vars: Vec<String>,
    schema: bool,
    limit: usize,
}

#[cfg(feature = "interactive")]
fn parse_repl_graph_args(args: Vec<&str>) -> Result<ReplGraphArgs> {
    let mut parsed = ReplGraphArgs {
        limit: 10,
        ..ReplGraphArgs::default()
    };
    let mut query = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let value = args[index];
        match value {
            "--schema" => parsed.schema = true,
            "-f" | "--file" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    bail!("{value} requires a file path");
                };
                parsed.file = Some(PathBuf::from(path));
            }
            "--var" => {
                index += 1;
                let Some(var) = args.get(index) else {
                    bail!("--var requires NAME=VALUE");
                };
                parsed.vars.push((*var).to_string());
            }
            "--limit" => {
                index += 1;
                let Some(limit) = args.get(index) else {
                    bail!("--limit requires a number");
                };
                parsed.limit = limit
                    .parse::<usize>()
                    .with_context(|| format!("invalid graph limit {limit:?}"))?
                    .max(1);
            }
            value if value.starts_with("--var=") => {
                parsed
                    .vars
                    .push(value.trim_start_matches("--var=").to_string());
            }
            value if value.starts_with("--limit=") => {
                let limit = value.trim_start_matches("--limit=");
                parsed.limit = limit
                    .parse::<usize>()
                    .with_context(|| format!("invalid graph limit {limit:?}"))?
                    .max(1);
            }
            value => {
                query.push(value.to_string());
                query.extend(args[index + 1..].iter().map(|value| (*value).to_string()));
                break;
            }
        }
        index += 1;
    }
    if !query.is_empty() {
        parsed.query = Some(query.join(" "));
    }
    if parsed.query.is_some() && parsed.file.is_some() {
        bail!("provide a graph query inline or with --file, not both");
    }
    Ok(parsed)
}

#[cfg(feature = "interactive")]
fn repl_graph_query_text(args: &ReplGraphArgs) -> Result<String> {
    query_text(args.query.clone(), args.file.clone(), "graph query")
}

#[cfg(feature = "interactive")]
fn graph_input_description(input: &GraphQlInput) -> Value {
    match input {
        GraphQlInput::String(value) => json!({"kind": "string", "value": value}),
        GraphQlInput::Int(value) => json!({"kind": "int", "value": value}),
        GraphQlInput::Bool(value) => json!({"kind": "bool", "value": value}),
        GraphQlInput::Variable(value) => json!({"kind": "variable", "name": value}),
        GraphQlInput::Null => json!({"kind": "null"}),
    }
}

#[cfg(feature = "interactive")]
fn graph_field_description(field: &GraphQlField) -> Value {
    json!({
        "responseKey": field.response_key,
        "field": field.name,
        "arguments": field.args.iter().map(|(name, value)| {
            json!({"name": name, "value": graph_input_description(value)})
        }).collect::<Vec<_>>(),
        "selection": field.selection.iter().map(graph_field_description).collect::<Vec<_>>(),
    })
}

#[cfg(feature = "interactive")]
fn graph_request_description(request: &GraphRequest) -> Value {
    match request {
        GraphRequest::Path { source, target } => json!({
            "type": "path",
            "source": source,
            "target": target,
        }),
        GraphRequest::WorksWith { left, right } => json!({
            "type": "worksWith",
            "left": left,
            "right": right,
        }),
        GraphRequest::Status { component } => json!({
            "type": "status",
            "component": component,
        }),
        GraphRequest::VersionsFor {
            component,
            for_component,
        } => json!({
            "type": "versions",
            "component": component,
            "for": for_component,
        }),
    }
}

#[cfg(feature = "interactive")]
fn repl_snippet_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "matrix", "matrix")
        .ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(dirs.config_dir().join("queries"))
}

#[cfg(feature = "interactive")]
fn sanitize_repl_snippet_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("snippet name cannot be empty");
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed == "." || trimmed == ".." {
        bail!("snippet names cannot include path separators");
    }
    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        bail!("snippet names can only use letters, numbers, '.', '-', and '_'");
    }
    Ok(trimmed.to_string())
}

#[cfg(feature = "interactive")]
fn repl_snippet_path(name: &str, query: Option<&str>) -> Result<PathBuf> {
    let name = sanitize_repl_snippet_name(name)?;
    let path = Path::new(&name);
    let name = if path.extension().is_some() {
        name
    } else if query.is_some_and(is_repl_graph_snippet) {
        format!("{name}.graphql")
    } else {
        format!("{name}.sql")
    };
    Ok(repl_snippet_dir()?.join(name))
}

#[cfg(feature = "interactive")]
fn is_repl_graph_snippet(query: &str) -> bool {
    is_native_graphql_query(query) || parse_graph_query(query).is_ok()
}

#[cfg(feature = "interactive")]
fn resolve_repl_snippet_path(name: &str) -> Result<PathBuf> {
    let exact = repl_snippet_path(name, None)?;
    if exact.exists() {
        return Ok(exact);
    }
    for extension in ["graphql", "gql", "sql"] {
        let candidate =
            repl_snippet_dir()?.join(format!("{}.{extension}", sanitize_repl_snippet_name(name)?));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("saved query {name:?} was not found")
}

#[cfg(feature = "interactive")]
fn list_repl_snippets_value() -> Result<Value> {
    let dir = repl_snippet_dir()?;
    let mut rows = Vec::new();
    if dir.exists() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                rows.push(json!({
                    "name": path.file_name().and_then(|value| value.to_str()).unwrap_or_default(),
                    "path": path.display().to_string(),
                    "kind": match path.extension().and_then(|value| value.to_str()) {
                        Some("graphql" | "gql") => "graphql",
                        Some("sql") => "sql",
                        _ => "query",
                    },
                }));
            }
        }
    }
    rows.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({
        "kind": "repl-snippets",
        "directory": dir.display().to_string(),
        "queries": rows,
    }))
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
    choices: Vec<ContextChoice>,
    db: Connection,
    cache: FactCacheSummary,
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
        let max_facts = matrix.max_facts(None)?;
        let sql_init = matrix.sql_init()?;
        let cached = load_query_db(matrix, max_facts, &context, FactLoadOptions::default()).await?;
        let fact_count = cached.cache.fact_count();
        Ok(Self {
            matrix,
            db: cached.db,
            context,
            sql_init,
            choices: Vec::new(),
            cache: cached.cache,
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
        self.reload(FactLoadOptions {
            policy: CachePolicy::Refresh,
        })
        .await
    }

    async fn reload(&mut self, options: FactLoadOptions) -> Result<()> {
        let cached = load_query_db(self.matrix, self.max_facts, &self.context, options).await?;
        self.fact_count = cached.cache.fact_count();
        self.db = cached.db;
        self.cache = cached.cache;
        self.last_refresh = SystemTime::now();
        Ok(())
    }

    fn rebuild_context(&mut self) -> Result<()> {
        prepare_query_views(&self.db, &self.context, self.sql_init.as_deref())?;
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

    fn run_sql_file(&self, path: &str) -> Result<()> {
        let sql =
            fs::read_to_string(path).with_context(|| format!("failed to read SQL file {path}"))?;
        self.run_sql(sql.trim().trim_end_matches(';').trim())
    }

    fn graph(&self) -> Result<GraphIndex> {
        GraphIndex::from_db(&self.db)
    }

    fn print_value(&self, value: &Value) -> Result<()> {
        match self.output_mode {
            OutputMode::Human => print_human_value(value),
            OutputMode::Json => print_json(value)?,
            OutputMode::Yaml => print_yaml(value)?,
            OutputMode::Csv | OutputMode::Table => print_generic_table(value),
        }
        Ok(())
    }

    fn run_graph_query(&self, query: &str, vars: &[String], limit: usize) -> Result<()> {
        let graph = self.graph()?;
        let value = if is_native_graphql_query(query) {
            let vars = parse_graphql_variables(vars)?;
            execute_graphql_document(&self.db, &graph, query, &vars, limit.max(1))?
        } else {
            let query = apply_graph_query_vars(query, vars)?;
            graph.execute_request(parse_graph_query(&query)?, limit.max(1))?
        };
        self.print_value(&value)
    }

    fn explain_graph_query(&self, query: &str, vars: &[String], limit: usize) -> Result<()> {
        let graph = self.graph()?;
        let variables = parse_graphql_variables(vars)?;
        let value = if is_native_graphql_query(query) {
            let fields = GraphQlParser::parse(query)?;
            let result = execute_graphql_document(&self.db, &graph, query, &variables, limit)?;
            json!({
                "kind": "graph-query-explain",
                "dialect": "graphql",
                "variables": variables,
                "rootFields": fields.iter().map(graph_field_description).collect::<Vec<_>>(),
                "resultKeys": result.get("data").and_then(Value::as_object).map(|data| {
                    data.keys().cloned().collect::<Vec<_>>()
                }).unwrap_or_default(),
            })
        } else {
            let query = apply_graph_query_vars(query, vars)?;
            let request = parse_graph_query(&query)?;
            let result = graph.execute_request(request.clone(), limit)?;
            json!({
                "kind": "graph-query-explain",
                "dialect": "matrix-graph-shorthand",
                "request": graph_request_description(&request),
                "resultKind": result.get("kind").cloned().unwrap_or(Value::Null),
                "status": result.get("status").cloned().unwrap_or(Value::Null),
                "recommended": result.get("recommended").cloned().unwrap_or(Value::Null),
            })
        };
        self.print_value(&value)
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
            "read" | "load" | "source" => {
                let path = parts.collect::<Vec<_>>().join(" ");
                if path.is_empty() {
                    eprintln!("Usage: .read <query.sql>");
                } else {
                    self.run_sql_file(&path)?;
                }
            }
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
            "compare" => {
                let args = parts.collect::<Vec<_>>();
                if args.is_empty() {
                    eprintln!("Usage: .compare <component|repo|subject> [--target-version <version>]");
                } else {
                    let (target, target_version) = parse_compare_repl_args(args)?;
                    self.run_sql(&compare_query_sql(&target, target_version.as_deref(), 50))?;
                }
            }
            "path" => {
                let args = parts.collect::<Vec<_>>();
                match args.as_slice() {
                    [source, target] => {
                        let value = self.graph()?.path_answer(source, target, 5)?;
                        self.print_value(&value)?;
                    }
                    [source, target, "--limit", limit] => {
                        let value = self.graph()?.path_answer(
                            source,
                            target,
                            limit.parse::<usize>().unwrap_or(5).max(1),
                        )?;
                        self.print_value(&value)?;
                    }
                    _ => eprintln!("Usage: .path <from-component> <to-component> [--limit N]"),
                }
            }
            "works-with" | "works" | "compatible" => {
                let args = parts.collect::<Vec<_>>();
                match args.as_slice() {
                    [left, right] => {
                        let value = self.graph()?.works_with_answer(left, right, 5)?;
                        self.print_value(&value)?;
                    }
                    [left, right, "--limit", limit] => {
                        let value = self.graph()?.works_with_answer(
                            left,
                            right,
                            limit.parse::<usize>().unwrap_or(5).max(1),
                        )?;
                        self.print_value(&value)?;
                    }
                    _ => eprintln!("Usage: .works-with <component-a> <component-b> [--limit N]"),
                }
            }
            "why" => {
                let args = parts.collect::<Vec<_>>();
                match args.as_slice() {
                    [left, right] => {
                        let mut value = self.graph()?.works_with_answer(left, right, 5)?;
                        if let Some(object) = value.as_object_mut() {
                            object.insert("kind".to_string(), json!("graph-why"));
                        }
                        self.print_value(&value)?;
                    }
                    _ => eprintln!("Usage: .why <component-a> <component-b>"),
                }
            }
            "resolve" => {
                let component = parts.collect::<Vec<_>>().join(" ");
                if component.is_empty() {
                    eprintln!("Usage: .resolve <component|repo|package|identity>");
                } else {
                    let value = self.graph()?.resolve_answer(&component)?;
                    self.print_value(&value)?;
                }
            }
            "graph" | "graphql" => {
                let args = parse_repl_graph_args(parts.collect::<Vec<_>>())?;
                if args.schema {
                    self.print_value(&json!({
                        "kind": "graphql-schema",
                        "schema": MATRIX_GRAPHQL_SCHEMA,
                    }))?;
                } else if args.query.is_none() && args.file.is_none() {
                    eprintln!("Usage: .graphql [--var name=value] [--limit N] <query> or .graphql -f <query.graphql>");
                } else {
                    let query = repl_graph_query_text(&args)?;
                    self.run_graph_query(&query, &args.vars, args.limit)?;
                }
            }
            "save" => {
                let mut args = parts.collect::<Vec<_>>();
                if args.len() < 2 {
                    eprintln!("Usage: .save <name> <sql-or-graph-query>");
                } else {
                    let name = args.remove(0);
                    let query = args.join(" ");
                    let path = repl_snippet_path(name, Some(&query))?;
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&path, query.trim())?;
                    eprintln!("Saved {}", path.display());
                }
            }
            "open" | "run" => {
                let mut args = parts.collect::<Vec<_>>();
                if args.is_empty() {
                    eprintln!("Usage: .open <name> [--var name=value] [--limit N]");
                } else {
                    let name = args.remove(0);
                    let path = resolve_repl_snippet_path(name)?;
                    let query = fs::read_to_string(&path)
                        .with_context(|| format!("failed to read saved query {}", path.display()))?;
                    let graph_args = parse_repl_graph_args(args)?;
                    match path.extension().and_then(|value| value.to_str()) {
                        Some("graphql" | "gql") => {
                            self.run_graph_query(&query, &graph_args.vars, graph_args.limit)?;
                        }
                        Some("sql") => self.run_sql(query.trim().trim_end_matches(';').trim())?,
                        _ if is_native_graphql_query(&query) => {
                            self.run_graph_query(&query, &graph_args.vars, graph_args.limit)?;
                        }
                        _ => self.run_sql(query.trim().trim_end_matches(';').trim())?,
                    }
                }
            }
            "snippets" | "queries" => {
                let value = list_repl_snippets_value()?;
                self.print_value(&value)?;
            }
            "producers" | "coverage" => {
                let value = producer_inventory_value(&self.db, &self.context, 50, 14)?;
                self.print_value(&value)?;
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
            "offline" => {
                self.reload(FactLoadOptions {
                    policy: CachePolicy::Offline,
                })
                .await?;
                eprintln!("Loaded {} cached facts.", self.fact_count);
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
                let args = parts.collect::<Vec<_>>();
                if matches!(args.first(), Some(&"graph" | &"graphql")) {
                    let graph_args = parse_repl_graph_args(args.into_iter().skip(1).collect())?;
                    if graph_args.query.is_none() && graph_args.file.is_none() {
                        eprintln!("Usage: .explain graph [--var name=value] <query>");
                    } else {
                        let query = repl_graph_query_text(&graph_args)?;
                        self.explain_graph_query(&query, &graph_args.vars, graph_args.limit)?;
                    }
                } else if args.is_empty() {
                    eprintln!("Usage: .explain select ... or .explain graph <query>");
                } else {
                    let sql = args.join(" ");
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
            "cache": self.cache.to_value(),
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

#[cfg(test)]
fn build_facts_db_with_init(
    facts: &[Value],
    context: &MatrixContext,
    sql_init: Option<&str>,
) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    populate_facts_table(&db, facts)?;
    prepare_query_views(&db, context, sql_init)?;
    Ok(db)
}

fn populate_facts_table(db: &Connection, facts: &[Value]) -> Result<()> {
    create_facts_table(db)?;
    insert_facts(db, facts)
}

fn create_facts_table(db: &Connection) -> Result<()> {
    db.execute_batch(
        "create table facts (
          id text, zone text, kind text, status text,
          type text, component text, canonical_component text, identity text,
          subject_class text, version text, repo text,
          source_repository text, source_repo text, source_sha text, source_ref text,
          subject_type text, subject_name text, channel text,
          tag text, observed_at text, accepted_at text,
          requires text, provides text, aliases text, json text not null
        );
        create index if not exists idx_matrix_facts_zone on facts(zone, observed_at);
        create index if not exists idx_matrix_facts_component on facts(component, repo, version);
        create index if not exists idx_matrix_facts_identity on facts(identity);
        create index if not exists idx_matrix_facts_status on facts(status);",
    )?;
    Ok(())
}

fn insert_facts(db: &Connection, facts: &[Value]) -> Result<()> {
    for record in facts {
        let fact = record
            .get("fact")
            .filter(|value| value.is_object())
            .unwrap_or(record);
        let zone = text_at(fact, &["track"])
            .or_else(|| text_at(record, &["track"]))
            .or_else(|| text_at(fact, &["zone"]))
            .or_else(|| text_at(record, &["zone"]));
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
    Ok(())
}

fn prepare_query_views(
    db: &Connection,
    context: &MatrixContext,
    sql_init: Option<&str>,
) -> Result<()> {
    drop_query_views(db)?;
    let zones = zones_from_db(db)?;
    create_matrix_views(db, context, &zones)?;
    if let Some(sql) = sql_init {
        apply_sql_init(db, sql)?;
    }
    Ok(())
}

fn zones_from_db(db: &Connection) -> Result<Vec<String>> {
    let mut stmt = db.prepare("select distinct zone from facts where zone is not null")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|zone| is_sql_identifier(zone))
        .collect())
}

fn drop_query_views(db: &Connection) -> Result<()> {
    let mut stmt = db.prepare("select name from sqlite_temp_master where type = 'view'")?;
    let views = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    for view in views {
        if is_sql_identifier(&view) {
            db.execute_batch(&format!(
                "drop view if exists temp.{};",
                quote_identifier(&view)
            ))?;
        }
    }
    Ok(())
}

fn create_matrix_views(db: &Connection, context: &MatrixContext, zones: &[String]) -> Result<()> {
    db.execute_batch(
        "create temp view zones as
          select zone, count(*) as facts,
                 sum(case when status in ('compatible', 'passed', 'observed', 'candidate') then 1 else 0 end) as valid,
                 sum(case when status in ('incompatible', 'failed') then 1 else 0 end) as invalid
          from facts
          where zone is not null
          group by zone;
        create temp view subjects as
          select type, component, canonical_component, identity, subject_class, repo, count(*) as facts,
                 max(observed_at) as last_observed_at
          from facts
          where component is not null
          group by type, component, canonical_component, identity, subject_class, repo;
        create temp view components as
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
        create temp view identities as
          select identity, canonical_component, subject_class,
                 min(type) as type,
                 min(subject_name) as subject_name,
                 min(repo) as repo,
                 count(*) as facts,
                 max(observed_at) as last_observed_at
          from facts
          where identity is not null
          group by identity, canonical_component, subject_class;
        create temp view identity_aliases as
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
        create temp view valid_facts as
          select * from facts
          where status in ('compatible', 'passed', 'observed', 'candidate', 'valid', 'ready');
        create temp view invalid_facts as
          select * from facts
          where status in ('incompatible', 'failed', 'invalid', 'blocked');
        create temp view requirements as
          select f.id as fact_id, f.zone, f.type, f.subject_class, f.component,
                 f.canonical_component, f.identity, f.subject_name, f.repo, f.version, f.status,
                 json_extract(item.value, '$.capability') as capability,
                 json_extract(item.value, '$.version') as capability_version,
                 item.value as requirement
          from facts f, json_each(coalesce(f.requires, '[]')) item;
        create temp view capabilities as
          select f.id as fact_id, f.zone, f.type, f.subject_class, f.component,
                 f.canonical_component, f.identity, f.subject_name, f.repo, f.version, f.status,
                 json_extract(item.value, '$.capability') as capability,
                 json_extract(item.value, '$.version') as capability_version,
                 item.value as provides
          from facts f, json_each(coalesce(f.provides, '[]')) item;
        create temp view members as
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
        create temp view deref as
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
        "create temp view context as select
           {} as zone,
           {} as repo,
           {} as component,
           {} as version,
           {} as tag,
           {} as sha,
           {} as ref;
         create temp view active as select * from facts where {active_where};
         create temp view current as select * from active;
         create temp view zone as select * from facts where {zone_where};
         create temp view upstream as
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
         create temp view downstream as
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
         create temp view compatible_with_current as
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
            "create temp view {} as select * from facts where zone = {};",
            quote_identifier(zone),
            sql_literal(zone)
        ))?;
    }

    Ok(())
}

fn apply_sql_init(db: &Connection, sql: &str) -> Result<()> {
    for statement in sql.split(';') {
        let statement = strip_sql_line_comments(statement).trim().to_string();
        let lower = statement.to_ascii_lowercase();
        if statement.is_empty() {
            continue;
        }
        let statement = if lower.starts_with("create temp view ")
            || lower.starts_with("create temporary view ")
        {
            statement
        } else if lower.starts_with("create view ") {
            format!("create temp view {}", &statement["create view ".len()..])
        } else {
            bail!("Matrix SQL init only allows CREATE VIEW statements");
        };
        db.execute_batch(&format!("{statement};"))
            .context("failed to apply Matrix SQL init views")?;
    }
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
  .offline                  Reload facts from the local persistent cache
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
  .path <from> <to>         Find an inferred compatibility path
  .works-with <a> <b>       Check whether two components connect
  .why <a> <b>              Explain pair compatibility
  .resolve <name>           Explain component/repo/package alias resolution
  .graphql <query>          Run native GraphQL or graph shorthand
  .graphql -f <file>        Run a graph query file
  .graphql --var n=v ...    Bind GraphQL variables
  .graphql --schema         Print the native Matrix GraphQL schema
  .save <name> <query>      Save a SQL or graph query snippet
  .open <name>              Run a saved query snippet
  .snippets                 List saved query snippets
  .producers                Show fact producer coverage and freshness
  .coverage                 Alias for .producers
  .read <file>              Run a saved SQL query file
  .examples                 Show copyable query examples
  .explain <sql>            Run EXPLAIN QUERY PLAN
  .explain graph <query>    Explain a graph or GraphQL query
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
    let examples = r#"Matrix SQL examples

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
.graphql --schema
.graphql --var component=eunomia --var for=aphrodite query Matrix($component:String!,$for:String!) { versions(component:$component, for:$for) { versions } }
.explain graph aphrodite -> eunomia
.save aphrodite-path { path(from:"aphrodite", to:"eunomia") { status paths { confidence nodes { component version } } } }
.open aphrodite-path
"#;
    println!("{examples}");
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
            ".graph",
            ".graphql",
            ".get",
            ".help",
            ".history",
            ".load",
            ".limit",
            ".members",
            ".mode",
            ".offline",
            ".open",
            ".path",
            ".read",
            ".refresh",
            ".resolve",
            ".run",
            ".schema",
            ".source",
            ".save",
            ".snippets",
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
            "--schema",
            "--var",
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
            "graph",
            "graphql",
            "group",
            "get",
            "history",
            "id",
            "incompatible",
            "json",
            "kind",
            "load",
            "limit",
            "producers",
            "members",
            "observed_at",
            "accepted_at",
            "order",
            "path",
            "provides",
            "read",
            "red",
            "repo",
            "resolve",
            "ref",
            "requirements",
            "requires",
            "select",
            "source",
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
    let reachability = if construct.is_some() {
        match matrix.get("").await {
            Ok(_) => json!({"reachable": true}),
            Err(error) => json!({"reachable": false, "error": error.to_string()}),
        }
    } else {
        json!({"reachable": false, "error": "no construct configured"})
    };
    Ok(json!({
        "configPath": matrix.config_path,
        "profile": matrix.profile.map(ConfigProfile::as_str),
        "construct": construct,
        "apiPrefix": matrix.api_prefix,
        "auth": auth_diagnostic(matrix),
        "reachable": reachability["reachable"].as_bool().unwrap_or(false),
        "reachability": reachability,
    }))
}

fn auth_diagnostic(matrix: &Matrix) -> Value {
    let Some(candidate) = matrix.auth_candidate() else {
        return json!({
            "configured": false,
            "available": false,
            "source": null,
        });
    };
    let (available, error) = match matrix.resolve_auth_candidate(&candidate) {
        Ok(Some(_)) => (true, None),
        Ok(None) => (false, None),
        Err(error) => (false, Some(error.to_string())),
    };
    json!({
        "configured": true,
        "available": available,
        "source": candidate.source(),
        "tokenFile": candidate.token_file(),
        "tokenCommand": candidate.token_command(),
        "error": error,
    })
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
        facts.extend(page_values(&body, "facts"));
        cursor = body["page"]["nextCursor"].as_str().map(ToString::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(facts)
}

async fn fetch_facts_head(matrix: &Matrix) -> Result<Option<FactsHead>> {
    let Some(body) = matrix.get_optional("/facts/head").await? else {
        return Ok(None);
    };
    let head: FactsHead =
        serde_json::from_value(body).context("construct returned an invalid facts head")?;
    if head.kind != "compatibility-facts-head" || head.digest.trim().is_empty() {
        bail!("construct returned an invalid facts head");
    }
    Ok(Some(head))
}

async fn sync_facts_to_cache(matrix: &Matrix, max_facts: usize) -> Result<FactCacheSummary> {
    let head = fetch_facts_head(matrix).await.ok().flatten();
    let facts = fetch_facts(matrix, max_facts).await?;
    write_fact_cache_db(matrix, &facts, max_facts, head.as_ref())
}

async fn cache_source_for_policy(
    matrix: &Matrix,
    path: &Path,
    max_facts: usize,
    policy: CachePolicy,
) -> Result<FactCacheSource> {
    match policy {
        CachePolicy::Offline => Ok(FactCacheSource::Cache),
        CachePolicy::Refresh => {
            sync_facts_to_cache(matrix, max_facts).await?;
            Ok(FactCacheSource::Live)
        }
        CachePolicy::Auto | CachePolicy::PreferCache => {
            if let Ok(summary) = fact_cache_summary_from_db(path, FactCacheSource::Cache)
                && summary.satisfies_max_facts(max_facts)
            {
                if policy == CachePolicy::PreferCache || !summary.stale {
                    return Ok(FactCacheSource::Cache);
                }
                if cache_head_matches(matrix, path, &summary).await? {
                    return Ok(FactCacheSource::Cache);
                }
            }
            sync_facts_to_cache(matrix, max_facts).await?;
            Ok(FactCacheSource::Live)
        }
    }
}

async fn cache_head_matches(
    matrix: &Matrix,
    path: &Path,
    summary: &FactCacheSummary,
) -> Result<bool> {
    let Some(metadata) = summary.metadata.as_ref() else {
        return Ok(false);
    };
    let Some(cached_digest) = metadata.head_digest.as_deref() else {
        return Ok(false);
    };
    let Some(head) = fetch_facts_head(matrix).await? else {
        return Ok(false);
    };
    if cached_digest != head.digest {
        return Ok(false);
    }
    if metadata
        .head_fact_count
        .is_some_and(|fact_count| fact_count != head.fact_count)
    {
        return Ok(false);
    }
    update_fact_cache_head_metadata(path, &head)?;
    Ok(true)
}

fn write_fact_cache_db(
    matrix: &Matrix,
    facts: &[Value],
    max_facts: usize,
    head: Option<&FactsHead>,
) -> Result<FactCacheSummary> {
    let final_path = fact_cache_path(matrix)?;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = final_path.with_extension(format!("sqlite.tmp.{}", process::id()));
    let _ = fs::remove_file(&temp_path);
    let db = Connection::open(&temp_path)?;
    db.execute_batch(
        "pragma journal_mode = delete;
         pragma synchronous = normal;",
    )?;
    populate_facts_table(&db, facts)?;
    let metadata = FactCacheMetadata {
        construct: matrix.construct.clone(),
        api_prefix: matrix.api_prefix.clone(),
        profile: matrix.profile,
        schema_version: 2,
        fetched_at_unix: unix_now()?,
        checked_at_unix: None,
        fact_count: facts.len(),
        max_facts,
        head_digest: head.map(|value| value.digest.clone()),
        head_fact_count: head.map(|value| value.fact_count),
        head_latest_accepted_at: head.and_then(|value| value.latest_accepted_at.clone()),
        head_latest_fact_id: head.and_then(|value| value.latest_fact_id.clone()),
        head_latest_content_hash: head.and_then(|value| value.latest_content_hash.clone()),
    };
    write_fact_cache_metadata(&db, &metadata)?;
    db.execute_batch("pragma optimize;")?;
    drop(db);
    fs::rename(&temp_path, &final_path).with_context(|| {
        format!(
            "failed to replace Matrix fact cache {}",
            final_path.display()
        )
    })?;
    Ok(FactCacheSummary::from_metadata(
        FactCacheSource::Live,
        final_path,
        metadata,
    ))
}

fn open_fact_cache_db(
    path: &Path,
    context: &MatrixContext,
    sql_init: Option<&str>,
) -> Result<Connection> {
    let db = Connection::open(path)
        .with_context(|| format!("failed to open Matrix SQLite fact cache {}", path.display()))?;
    let metadata = read_fact_cache_metadata(&db)?;
    if metadata.schema_version != 2 {
        bail!(
            "unsupported Matrix SQLite fact cache schema {} in {}; run `matrix sync`",
            metadata.schema_version,
            path.display()
        );
    }
    prepare_query_views(&db, context, sql_init)?;
    Ok(db)
}

fn write_fact_cache_metadata(db: &Connection, metadata: &FactCacheMetadata) -> Result<()> {
    db.execute_batch(
        "create table matrix_cache_metadata (
           key text primary key,
           value text not null
         );",
    )?;
    let fields = [
        ("construct", metadata.construct.clone().unwrap_or_default()),
        ("apiPrefix", metadata.api_prefix.clone()),
        (
            "profile",
            metadata
                .profile
                .map(ConfigProfile::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        ("schemaVersion", metadata.schema_version.to_string()),
        ("fetchedAtUnix", metadata.fetched_at_unix.to_string()),
        (
            "checkedAtUnix",
            metadata
                .checked_at_unix
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        ("factCount", metadata.fact_count.to_string()),
        ("maxFacts", metadata.max_facts.to_string()),
        (
            "headDigest",
            metadata.head_digest.clone().unwrap_or_default(),
        ),
        (
            "headFactCount",
            metadata
                .head_fact_count
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "headLatestAcceptedAt",
            metadata.head_latest_accepted_at.clone().unwrap_or_default(),
        ),
        (
            "headLatestFactId",
            metadata.head_latest_fact_id.clone().unwrap_or_default(),
        ),
        (
            "headLatestContentHash",
            metadata
                .head_latest_content_hash
                .clone()
                .unwrap_or_default(),
        ),
    ];
    for (key, value) in fields {
        db.execute(
            "insert into matrix_cache_metadata (key, value) values (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

fn read_fact_cache_metadata(db: &Connection) -> Result<FactCacheMetadata> {
    let mut stmt = db.prepare("select key, value from matrix_cache_metadata")?;
    let entries = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<HashMap<_, _>, _>>()?;
    let profile = entries
        .get("profile")
        .filter(|value| !value.is_empty())
        .and_then(|value| ConfigProfile::from_str(value, true).ok());
    Ok(FactCacheMetadata {
        construct: entries
            .get("construct")
            .filter(|value| !value.is_empty())
            .cloned(),
        api_prefix: entries
            .get("apiPrefix")
            .cloned()
            .unwrap_or_else(|| "/v1/matrix".to_string()),
        profile,
        schema_version: entries
            .get("schemaVersion")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        fetched_at_unix: entries
            .get("fetchedAtUnix")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        checked_at_unix: entries
            .get("checkedAtUnix")
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse().ok()),
        fact_count: entries
            .get("factCount")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        max_facts: entries
            .get("maxFacts")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        head_digest: entries
            .get("headDigest")
            .filter(|value| !value.is_empty())
            .cloned(),
        head_fact_count: entries
            .get("headFactCount")
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse().ok()),
        head_latest_accepted_at: entries
            .get("headLatestAcceptedAt")
            .filter(|value| !value.is_empty())
            .cloned(),
        head_latest_fact_id: entries
            .get("headLatestFactId")
            .filter(|value| !value.is_empty())
            .cloned(),
        head_latest_content_hash: entries
            .get("headLatestContentHash")
            .filter(|value| !value.is_empty())
            .cloned(),
    })
}

fn update_fact_cache_head_metadata(path: &Path, head: &FactsHead) -> Result<()> {
    let db = Connection::open(path)?;
    let fields = [
        ("checkedAtUnix", unix_now()?.to_string()),
        ("headDigest", head.digest.clone()),
        ("headFactCount", head.fact_count.to_string()),
        (
            "headLatestAcceptedAt",
            head.latest_accepted_at.clone().unwrap_or_default(),
        ),
        (
            "headLatestFactId",
            head.latest_fact_id.clone().unwrap_or_default(),
        ),
        (
            "headLatestContentHash",
            head.latest_content_hash.clone().unwrap_or_default(),
        ),
    ];
    for (key, value) in fields {
        db.execute(
            "insert or replace into matrix_cache_metadata (key, value) values (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

fn fact_cache_summary_from_db(path: &Path, source: FactCacheSource) -> Result<FactCacheSummary> {
    let db = Connection::open(path)?;
    let metadata = read_fact_cache_metadata(&db)?;
    Ok(FactCacheSummary::from_metadata(
        source,
        path.to_path_buf(),
        metadata,
    ))
}

fn fact_cache_status(matrix: &Matrix) -> Result<Value> {
    let path = fact_cache_path(matrix)?;
    let policy = matrix.cache_policy()?;
    let summary = match fact_cache_summary_from_db(&path, FactCacheSource::Cache) {
        Ok(cache) => cache.with_policy(policy),
        Err(_) => FactCacheSummary {
            source: FactCacheSource::Missing,
            policy,
            path,
            metadata: None,
            age_seconds: None,
            checked_age_seconds: None,
            stale: false,
        },
    };
    Ok(summary.to_value())
}

fn clear_fact_cache(matrix: &Matrix, all: bool) -> Result<Value> {
    let root = fact_cache_dir()?;
    let mut removed = 0usize;
    if all {
        if root.exists() {
            for entry in fs::read_dir(&root)? {
                let entry = entry?;
                let path = entry.path();
                if matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("sqlite" | "json")
                ) {
                    fs::remove_file(&path)?;
                    removed += 1;
                }
            }
        }
    } else {
        let path = fact_cache_path(matrix)?;
        if path.exists() {
            fs::remove_file(&path)?;
            removed = 1;
        }
    }
    Ok(json!({
        "kind": "fact-cache-clear",
        "removed": removed,
        "path": if all { Value::Null } else { json!(fact_cache_path(matrix)?.display().to_string()) },
        "all": all,
    }))
}

fn fact_cache_dir() -> Result<PathBuf> {
    Ok(cache_root()?.join("facts"))
}

fn fact_cache_path(matrix: &Matrix) -> Result<PathBuf> {
    Ok(fact_cache_dir()?.join(format!("{}.sqlite", fact_cache_key(matrix))))
}

fn fact_cache_key(matrix: &Matrix) -> String {
    let mut hasher = DefaultHasher::new();
    matrix.profile.map(ConfigProfile::as_str).hash(&mut hasher);
    matrix.construct.hash(&mut hasher);
    matrix.api_prefix.hash(&mut hasher);
    let prefix = matrix
        .profile
        .map(|profile| format!("profile-{}", profile.as_str()))
        .unwrap_or_else(|| "custom".to_string());
    format!("{prefix}-{:016x}", hasher.finish())
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_secs())
}

fn unix_age_seconds(fetched_at_unix: u64) -> Option<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(fetched_at_unix))
}

fn with_cache_summary(mut value: Value, cache: &FactCacheSummary) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("cache".to_string(), cache.to_value());
    }
    value
}

impl FactCacheSummary {
    fn from_metadata(source: FactCacheSource, path: PathBuf, metadata: FactCacheMetadata) -> Self {
        let age_seconds = unix_age_seconds(metadata.fetched_at_unix);
        let checked_at_unix = metadata.checked_at_unix.unwrap_or(metadata.fetched_at_unix);
        let checked_age_seconds = unix_age_seconds(checked_at_unix);
        Self {
            source,
            policy: CachePolicy::Auto,
            path,
            metadata: Some(metadata),
            age_seconds,
            checked_age_seconds,
            stale: checked_age_seconds.is_some_and(|age| age > FACT_CACHE_STALE_AFTER.as_secs()),
        }
    }

    fn with_policy(mut self, policy: CachePolicy) -> Self {
        self.policy = policy;
        self
    }

    fn to_value(&self) -> Value {
        let metadata = self.metadata.as_ref();
        let size_bytes = fs::metadata(&self.path).ok().map(|metadata| metadata.len());
        json!({
            "source": self.source.as_str(),
            "policy": self.policy.as_str(),
            "path": self.path.display().to_string(),
            "exists": self.source != FactCacheSource::Missing,
            "format": if self.source == FactCacheSource::Missing { Value::Null } else { json!("sqlite") },
            "sizeBytes": size_bytes,
            "sizeHuman": size_bytes.map(human_bytes),
            "construct": metadata.and_then(|value| value.construct.clone()),
            "apiPrefix": metadata.map(|value| value.api_prefix.clone()),
            "profile": metadata.and_then(|value| value.profile.map(ConfigProfile::as_str)),
            "schemaVersion": metadata.map(|value| value.schema_version),
            "fetchedAtUnix": metadata.map(|value| value.fetched_at_unix),
            "fetchedAt": metadata.map(|value| unix_timestamp_human(value.fetched_at_unix)),
            "checkedAtUnix": metadata.map(|value| value.checked_at_unix.unwrap_or(value.fetched_at_unix)),
            "checkedAt": metadata.map(|value| unix_timestamp_human(value.checked_at_unix.unwrap_or(value.fetched_at_unix))),
            "ageSeconds": self.age_seconds,
            "ageHuman": self.age_seconds.map(human_duration),
            "checkedAgeSeconds": self.checked_age_seconds,
            "checkedAgeHuman": self.checked_age_seconds.map(human_duration),
            "staleAfterSeconds": FACT_CACHE_STALE_AFTER.as_secs(),
            "stale": self.stale,
            "factCount": metadata.map(|value| value.fact_count),
            "maxFacts": metadata.map(|value| value.max_facts),
            "headDigest": metadata.and_then(|value| value.head_digest.clone()),
            "headFactCount": metadata.and_then(|value| value.head_fact_count),
            "headLatestAcceptedAt": metadata.and_then(|value| value.head_latest_accepted_at.clone()),
            "headLatestFactId": metadata.and_then(|value| value.head_latest_fact_id.clone()),
            "headLatestContentHash": metadata.and_then(|value| value.head_latest_content_hash.clone()),
        })
    }

    fn satisfies_max_facts(&self, requested: usize) -> bool {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.max_facts >= requested)
            .unwrap_or(false)
    }

    #[cfg(feature = "interactive")]
    fn fact_count(&self) -> usize {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.fact_count)
            .unwrap_or(0)
    }
}

impl FactCacheSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Cache => "cache",
            Self::Missing => "missing",
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn human_duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    format!("{days}d")
}

fn unix_timestamp_human(seconds: u64) -> String {
    seconds.to_string()
}

fn page_values(body: &Value, primary_key: &str) -> Vec<Value> {
    body.get(primary_key)
        .or_else(|| body.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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
    let output = if is_query_result(value) && value.get("cache").is_none() {
        query_rows(value)
    } else {
        normalize_query_result_value(value)
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn print_yaml(value: &Value) -> Result<()> {
    let output = if is_query_result(value) && value.get("cache").is_none() {
        query_rows(value)
    } else {
        normalize_query_result_value(value)
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

fn normalize_query_result_value(value: &Value) -> Value {
    if !is_query_result(value) {
        return value.clone();
    }
    let mut object = value.as_object().cloned().unwrap_or_default();
    object.insert("rows".to_string(), query_rows(value));
    Value::Object(object)
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
        let mut text = "No rows.\nTry `matrix components`, `matrix versions`, or `matrix query 'select * from context'` to inspect the active context.\n".to_string();
        if let Some(cache) = value.get("cache").and_then(Value::as_object) {
            text.push_str(&cache_human_line(cache));
        }
        return text;
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
    if let Some(cache) = value.get("cache").and_then(Value::as_object) {
        text.push_str(&cache_human_line(cache));
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
    if object
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "graphql-schema")
    {
        if let Some(schema) = object.get("schema").and_then(Value::as_str) {
            println!("{schema}");
        }
        return;
    }
    if object
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("graph-"))
    {
        print!("{}", graph_answer_human_text(object));
        return;
    }
    if object
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "producer-inventory")
    {
        print!("{}", producer_inventory_human_text(object));
        return;
    }
    if object
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("fact-cache-"))
    {
        print_cache_command_human(object);
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

fn print_cache_command_human(object: &serde_json::Map<String, Value>) {
    match object.get("kind").and_then(Value::as_str) {
        Some("fact-cache-clear") => {
            let removed = object.get("removed").and_then(Value::as_u64).unwrap_or(0);
            println!(
                "Removed {removed} cache file{}.",
                if removed == 1 { "" } else { "s" }
            );
        }
        Some("fact-cache-sync") | Some("fact-cache-status") => {
            println!("Matrix fact cache");
            if let Some(cache) = object.get("cache").and_then(Value::as_object) {
                for key in [
                    "source",
                    "policy",
                    "exists",
                    "format",
                    "sizeHuman",
                    "profile",
                    "construct",
                    "apiPrefix",
                    "schemaVersion",
                    "factCount",
                    "maxFacts",
                    "checkedAgeHuman",
                    "checkedAgeSeconds",
                    "ageHuman",
                    "ageSeconds",
                    "staleAfterSeconds",
                    "stale",
                    "headDigest",
                    "headFactCount",
                    "headLatestFactId",
                    "headLatestAcceptedAt",
                    "path",
                ] {
                    if let Some(value) = cache.get(key) {
                        println!("{}: {}", human_label(key), human_inline_value(value));
                    }
                }
            }
        }
        _ => {
            for (key, value) in object {
                println!("{}: {}", human_label(key), human_inline_value(value));
            }
        }
    }
}

fn graph_answer_human_text(object: &serde_json::Map<String, Value>) -> String {
    let mut text = match object.get("kind").and_then(Value::as_str) {
        Some("graph-path") => graph_path_human_text(object, "Path"),
        Some("graph-works-with") => graph_path_human_text(object, "Works with"),
        Some("graph-why") => graph_path_human_text(object, "Why"),
        Some("graph-status") => graph_status_human_text(object),
        Some("graph-versions-for") => graph_versions_human_text(object),
        Some("graph-resolve") => graph_resolve_human_text(object),
        _ => {
            let mut text = String::new();
            for (key, value) in object {
                text.push_str(&format!(
                    "{}: {}\n",
                    human_label(key),
                    human_inline_value(value)
                ));
            }
            text
        }
    };
    if let Some(cache) = object.get("cache").and_then(Value::as_object) {
        text.push_str(&cache_human_line(cache));
    }
    text
}

fn cache_human_line(cache: &serde_json::Map<String, Value>) -> String {
    let source = cache
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let facts = cache
        .get("factCount")
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let age = cache
        .get("ageHuman")
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .or_else(|| {
            cache
                .get("ageSeconds")
                .and_then(Value::as_u64)
                .map(human_duration)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let stale = cache.get("stale").and_then(Value::as_bool).unwrap_or(false);
    let checked = cache
        .get("checkedAgeHuman")
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .or_else(|| {
            cache
                .get("checkedAgeSeconds")
                .and_then(Value::as_u64)
                .map(human_duration)
        });
    let mut line = format!("Cache: {source}, {facts} facts, last refreshed {age} ago");
    if let Some(checked) = checked.filter(|value| value != &age) {
        line.push_str(&format!(", checked {checked} ago"));
    }
    line.push('\n');
    if stale && source == "cache" {
        line.push_str(
            "Warning: using stale local Matrix cache; run `matrix sync` or add `--refresh-cache` for fresh facts.\n",
        );
    }
    line
}

fn graph_path_human_text(object: &serde_json::Map<String, Value>, title: &str) -> String {
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let paths = object
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let confidence = object
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut text = format!("{title}: {status} ({confidence} confidence)\n");
    if paths.is_empty() {
        text.push_str("No known compatibility path.\n");
        if let Some(missing) = object.get("missing").and_then(Value::as_array) {
            for item in missing {
                text.push_str(&format!("- {}\n", human_inline_value(item)));
            }
        }
        return text;
    }

    for (index, path) in paths.iter().enumerate() {
        let nodes = path
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let node_labels = nodes.iter().map(graph_node_label).collect::<Vec<_>>();
        text.push_str(&format!(
            "{}. {}",
            index + 1,
            if node_labels.is_empty() {
                "same component".to_string()
            } else {
                node_labels.join(" -> ")
            }
        ));
        if let Some(confidence) = path.get("confidence").and_then(Value::as_str) {
            text.push_str(&format!(" [{confidence}]"));
        }
        if let Some(score) = path.get("score").and_then(Value::as_i64) {
            text.push_str(&format!(" score {score}"));
        }
        text.push('\n');
        if let Some(edges) = path.get("edges").and_then(Value::as_array) {
            for edge in edges {
                text.push_str(&format!("   {}\n", graph_edge_label(edge)));
            }
        }
    }
    text
}

fn graph_status_human_text(object: &serde_json::Map<String, Value>) -> String {
    let component = object
        .get("component")
        .map(graph_node_label)
        .unwrap_or_else(|| "unknown".to_string());
    let outgoing_count = object
        .get("outgoingCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let incoming_count = object
        .get("incomingCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut text =
        format!("Status: {component}\nOutgoing: {outgoing_count}\nIncoming: {incoming_count}\n");
    for (label, key) in [("Outgoing", "outgoing"), ("Incoming", "incoming")] {
        let edges = object
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if edges.is_empty() {
            continue;
        }
        text.push_str(&format!("{label} edges\n"));
        for edge in edges {
            text.push_str(&format!("- {}\n", graph_edge_label(&edge)));
        }
    }
    text
}

fn graph_versions_human_text(object: &serde_json::Map<String, Value>) -> String {
    let component = object
        .get("component")
        .map(graph_node_label)
        .unwrap_or_else(|| "unknown".to_string());
    let for_component = object
        .get("for")
        .map(graph_node_label)
        .unwrap_or_else(|| "unknown".to_string());
    let versions = object
        .get("versions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut text = format!("Versions: {component} for {for_component}\n");
    if versions.is_empty() {
        text.push_str("No known matching versions.\n");
    } else {
        let candidates = object
            .get("versionCandidates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for version in versions {
            let details = candidates
                .iter()
                .find(|candidate| candidate.get("version") == Some(&version));
            if let Some(details) = details {
                let confidence = details
                    .get("confidence")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let score = details.get("score").and_then(Value::as_i64).unwrap_or(0);
                text.push_str(&format!(
                    "- {} ({confidence}, score {score})\n",
                    human_inline_value(&version)
                ));
            } else {
                text.push_str(&format!("- {}\n", human_inline_value(&version)));
            }
        }
    }
    text
}

fn producer_inventory_human_text(object: &serde_json::Map<String, Value>) -> String {
    let summary = object.get("summary").and_then(Value::as_object);
    let producers = summary
        .and_then(|value| value.get("producers"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let stale = summary
        .and_then(|value| value.get("staleProducers"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let facts = summary
        .and_then(|value| value.get("facts"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let invalid = summary
        .and_then(|value| value.get("invalidFacts"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let missing_metadata = summary
        .and_then(|value| value.get("missingProducerMetadataFacts"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut text = format!(
        "Producer inventory: {producers} producers, {facts} facts, {stale} stale, {invalid} invalid, {missing_metadata} missing producer metadata\n"
    );
    for row in object
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let producer = row
            .get("producer")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let facts = row.get("facts").and_then(Value::as_i64).unwrap_or(0);
        let components = row.get("components").and_then(Value::as_i64).unwrap_or(0);
        let freshness = row
            .get("freshness")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let metadata = row
            .get("producer_metadata")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        text.push_str(&format!(
            "- {producer}: {facts} facts, {components} components, {freshness}, {metadata}\n"
        ));
    }
    if let Some(cache) = object.get("cache").and_then(Value::as_object) {
        text.push_str(&cache_human_line(cache));
    }
    text
}

fn graph_resolve_human_text(object: &serde_json::Map<String, Value>) -> String {
    let requested = object
        .get("requested")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let resolved = object
        .get("resolved")
        .map(graph_node_label)
        .unwrap_or_else(|| "unknown".to_string());
    let ambiguous = object
        .get("ambiguous")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut text = format!("Resolve: {requested} -> {resolved}\n");
    if ambiguous {
        text.push_str("Ambiguous: yes\n");
    }
    if let Some(warnings) = object.get("warnings").and_then(Value::as_array)
        && !warnings.is_empty()
    {
        text.push_str("Warnings\n");
        for warning in warnings {
            text.push_str(&format!("- {}\n", human_inline_value(warning)));
        }
    }
    if let Some(matches) = object.get("matches").and_then(Value::as_array)
        && !matches.is_empty()
    {
        text.push_str("Matches\n");
        for item in matches {
            let node = item
                .get("node")
                .map(graph_node_label)
                .unwrap_or_else(|| human_inline_value(item));
            let kinds = item
                .get("aliasKinds")
                .map(human_inline_value)
                .filter(|value| !value.is_empty() && value != "-")
                .unwrap_or_else(|| "alias".to_string());
            text.push_str(&format!("- {node} ({kinds})\n"));
        }
    }
    text
}

fn graph_node_label(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return human_inline_value(value);
    };
    let component = object
        .get("component")
        .and_then(Value::as_str)
        .or_else(|| object.get("key").and_then(Value::as_str))
        .unwrap_or("unknown");
    if let Some(version) = object.get("version").and_then(Value::as_str) {
        format!("{component} {version}")
    } else {
        component.to_string()
    }
}

fn graph_edge_label(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return human_inline_value(value);
    };
    let from = object
        .get("from")
        .map(graph_node_label)
        .unwrap_or_else(|| "?".to_string());
    let to = object
        .get("to")
        .map(graph_node_label)
        .unwrap_or_else(|| "?".to_string());
    let relationship = object
        .get("relationship")
        .and_then(Value::as_str)
        .unwrap_or("relates-to");
    let mut label = format!("{from} {relationship} {to}");
    if let Some(capability) = object.get("capability").and_then(Value::as_str) {
        label.push_str(&format!(" via {capability}"));
        if let Some(version) = object.get("capabilityVersion").and_then(Value::as_str) {
            label.push_str(&format!(" {version}"));
        }
    }
    if let Some(status) = object.get("status").and_then(Value::as_str) {
        label.push_str(&format!(" [{status}]"));
    }
    if let Some(fact_id) = object.get("sourceFactId").and_then(Value::as_str) {
        label.push_str(&format!(" ({fact_id})"));
    }
    label
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

fn query_suffix(values: Vec<(&str, String)>) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("?{}", query_string(values))
    }
}

fn push_query(query: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        query.push((key, value));
    }
}

fn push_page_query(query: &mut Vec<(&'static str, String)>, page: PageArgs) {
    if let Some(limit) = page.limit {
        query.push(("limit", limit.to_string()));
    }
    push_query(query, "cursor", page.cursor);
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

    fn test_matrix(config: Config, profile: Option<ConfigProfile>) -> Matrix {
        Matrix {
            config_path: PathBuf::from("/tmp/matrix-test-config.json"),
            config,
            profile,
            construct: Some("https://matrix.example.test".to_string()),
            api_prefix: "/v1/compatibility".to_string(),
            output: OutputFormat::Json,
            client: reqwest::Client::new(),
        }
    }

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
    fn accepts_query_file_input() {
        let cli = Cli::try_parse_from(["matrix", "query", "-f", "queries/current.sql"]).unwrap();
        match cli.command {
            Commands::Query(args) => {
                assert!(args.sql.is_none());
                assert_eq!(args.file, Some(PathBuf::from("queries/current.sql")));
            }
            _ => panic!("expected query command"),
        }
    }

    #[test]
    fn accepts_fact_cache_commands_and_offline_flags() {
        let sync = Cli::try_parse_from(["matrix", "sync", "--max-facts", "250"]).unwrap();
        match sync.command {
            Commands::Sync(args) => assert_eq!(args.max_facts, Some(250)),
            _ => panic!("expected sync command"),
        }

        let status = Cli::try_parse_from(["matrix", "cache", "status"]).unwrap();
        match status.command {
            Commands::Cache(CacheCommand {
                command: CacheSubcommand::Status,
            }) => {}
            _ => panic!("expected cache status command"),
        }

        let clear = Cli::try_parse_from(["matrix", "cache", "clear", "--all"]).unwrap();
        match clear.command {
            Commands::Cache(CacheCommand {
                command: CacheSubcommand::Clear { all },
            }) => assert!(all),
            _ => panic!("expected cache clear command"),
        }

        let query = Cli::try_parse_from([
            "matrix",
            "query",
            "select * from facts",
            "--offline",
            "--refresh-cache",
        ])
        .unwrap();
        match query.command {
            Commands::Query(args) => {
                assert!(args.cache.offline);
                assert!(args.cache.refresh_cache);
            }
            _ => panic!("expected query command"),
        }

        let path =
            Cli::try_parse_from(["matrix", "path", "aphrodite", "eunomia", "--offline"]).unwrap();
        match path.command {
            Commands::Path(args) => assert!(args.cache.offline),
            _ => panic!("expected path command"),
        }

        let versions = Cli::try_parse_from([
            "matrix",
            "versions",
            "eunomia",
            "--for",
            "aphrodite",
            "--offline",
        ])
        .unwrap();
        match versions.command {
            Commands::Versions(args) => assert!(args.cache.offline),
            _ => panic!("expected versions command"),
        }
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
                assert_eq!(args.max_facts, Some(10000));
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
    fn accepts_graph_answer_commands() {
        let path = Cli::try_parse_from(["matrix", "path", "aphrodite", "eunomia"]).unwrap();
        match path.command {
            Commands::Path(args) => {
                assert_eq!(args.source, "aphrodite");
                assert_eq!(args.target, "eunomia");
            }
            _ => panic!("expected path command"),
        }

        let works_with =
            Cli::try_parse_from(["matrix", "works-with", "putto", "aphrodite"]).unwrap();
        match works_with.command {
            Commands::WorksWith(args) => {
                assert_eq!(args.left, "putto");
                assert_eq!(args.right, "aphrodite");
            }
            _ => panic!("expected works-with command"),
        }

        let compatible =
            Cli::try_parse_from(["matrix", "compatible", "aphrodite", "putto"]).unwrap();
        match compatible.command {
            Commands::Compatible(args) => {
                assert_eq!(args.left.as_deref(), Some("aphrodite"));
                assert_eq!(args.right.as_deref(), Some("putto"));
            }
            _ => panic!("expected compatible command"),
        }

        let versions =
            Cli::try_parse_from(["matrix", "versions", "putto", "--for", "aphrodite"]).unwrap();
        match versions.command {
            Commands::Versions(args) => {
                assert_eq!(args.component_filter.as_deref(), Some("putto"));
                assert_eq!(args.for_component.as_deref(), Some("aphrodite"));
            }
            _ => panic!("expected versions command"),
        }

        let why = Cli::try_parse_from(["matrix", "why", "aphrodite", "eunomia"]).unwrap();
        match why.command {
            Commands::Why(args) => {
                assert_eq!(args.left, "aphrodite");
                assert_eq!(args.right, "eunomia");
            }
            _ => panic!("expected why command"),
        }

        let graph = Cli::try_parse_from(["matrix", "graph", "aphrodite -> eunomia"]).unwrap();
        match graph.command {
            Commands::Graph(args) => {
                assert_eq!(args.query.as_deref(), Some("aphrodite -> eunomia"))
            }
            _ => panic!("expected graph command"),
        }

        let graph_file =
            Cli::try_parse_from(["matrix", "graphql", "-f", "queries/path.graphql"]).unwrap();
        match graph_file.command {
            Commands::Graph(args) => {
                assert!(args.query.is_none());
                assert_eq!(args.file, Some(PathBuf::from("queries/path.graphql")));
            }
            _ => panic!("expected graph command"),
        }

        let graphql = Cli::try_parse_from([
            "matrix",
            "graphql",
            "{ path(from:\"aphrodite\", to:\"eunomia\") { status } }",
            "-o",
            "json",
        ])
        .unwrap();
        assert_eq!(graphql.output, OutputFormat::Json);
        match graphql.command {
            Commands::Graph(args) => assert!(args.query.as_deref().unwrap().contains("path")),
            _ => panic!("expected graphql alias command"),
        }

        let resolve = Cli::try_parse_from(["matrix", "resolve", "aphrodite"]).unwrap();
        match resolve.command {
            Commands::Resolve(args) => assert_eq!(args.component, "aphrodite"),
            _ => panic!("expected resolve command"),
        }
    }

    #[test]
    fn accepts_red_wiz_profile_and_compatibility_commands() {
        let profiled =
            Cli::try_parse_from(["matrix", "--profile", "red-wiz", "capabilities"]).unwrap();
        assert_eq!(profiled.profile, Some(ConfigProfile::RedWiz));
        match profiled.command {
            Commands::Capabilities => {}
            _ => panic!("expected capabilities command"),
        }

        let scopes = Cli::try_parse_from(["matrix", "--profile", "red-wiz", "scopes"]).unwrap();
        match scopes.command {
            Commands::Scopes => {}
            _ => panic!("expected scopes command"),
        }

        let scope = Cli::try_parse_from(["matrix", "scope", "agent-admin/native-askar"]).unwrap();
        match scope.command {
            Commands::Scope { scope_id } => {
                assert_eq!(scope_id, "agent-admin/native-askar");
            }
            _ => panic!("expected scope command"),
        }

        let config = Cli::try_parse_from(["matrix", "config", "use", "red-wiz"]).unwrap();
        match config.command {
            Commands::Config(command) => match command.command {
                ConfigSubcommand::Use { profile } => {
                    assert_eq!(profile, ConfigProfile::RedWiz);
                    assert_eq!(profile.construct(), RED_WIZ_CONSTRUCT_URL);
                    assert_eq!(profile.api_prefix(), RED_WIZ_API_PREFIX);
                    assert_eq!(profile.token_command(), Some(RED_WIZ_TOKEN_COMMAND));
                }
                _ => panic!("expected config use command"),
            },
            _ => panic!("expected config command"),
        }

        let artifacts = Cli::try_parse_from([
            "matrix",
            "artifacts",
            "--track",
            "odin",
            "--subject-type",
            "npm",
            "--limit",
            "10",
        ])
        .unwrap();
        match artifacts.command {
            Commands::Artifacts(args) => {
                assert_eq!(args.track.as_deref(), Some("odin"));
                assert_eq!(args.subject_type.as_deref(), Some("npm"));
                assert_eq!(args.page.limit, Some(10));
            }
            _ => panic!("expected artifacts command"),
        }

        let blockers =
            Cli::try_parse_from(["matrix", "blockers", "agent-admin", "--environment", "qa"])
                .unwrap();
        match blockers.command {
            Commands::Blockers(args) => {
                assert_eq!(args.track, "agent-admin");
                assert_eq!(args.environment.as_deref(), Some("qa"));
            }
            _ => panic!("expected blockers command"),
        }
    }

    #[test]
    fn extracts_token_command_output_from_raw_and_json() {
        let raw = token_from_command_stdout("test", b"  raw-token\n".to_vec()).unwrap();
        assert_eq!(raw.as_deref(), Some("raw-token"));

        let credential_process = token_from_command_stdout(
            "test",
            br#"{"access_token":"json-token","expires_at":"2026-06-25T12:34:56Z"}"#.to_vec(),
        )
        .unwrap();
        assert_eq!(credential_process.as_deref(), Some("json-token"));
    }

    #[test]
    fn rejects_json_token_command_without_a_real_token() {
        let err = token_from_command_stdout(
            RED_WIZ_TOKEN_COMMAND,
            br#"{"environment":[{"name":"MATRIX_TOKEN","secret":true,"value":"wiz-token"}]}"#
                .to_vec(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("emitted JSON without a token"));
    }

    #[test]
    fn auth_diagnostic_reports_source_without_leaking_config_token() {
        let matrix = test_matrix(
            Config {
                token: Some("secret-token".to_string()),
                ..Config::default()
            },
            None,
        );

        let diagnostic = auth_diagnostic(&matrix);
        assert_eq!(diagnostic["configured"], true);
        assert_eq!(diagnostic["available"], true);
        assert_eq!(diagnostic["source"], "config-token");
        assert!(
            !serde_json::to_string(&diagnostic)
                .unwrap()
                .contains("secret-token")
        );
    }

    #[test]
    fn red_wiz_profile_selects_profile_token_command_candidate() {
        let matrix = test_matrix(Config::default(), Some(ConfigProfile::RedWiz));

        let candidate = matrix.auth_candidate().expect("profile token command");
        assert_eq!(candidate.source(), "profile-token-command");
        assert_eq!(candidate.token_command(), Some(RED_WIZ_TOKEN_COMMAND));
    }

    #[tokio::test]
    async fn config_use_red_wiz_replaces_stored_token_with_profile_command() {
        let mut matrix = test_matrix(
            Config {
                token: Some("stale-token".to_string()),
                token_file: Some("/tmp/stale-matrix-token".to_string()),
                token_command: Some("old-command".to_string()),
                ..Config::default()
            },
            None,
        );
        matrix.config_path = env::temp_dir().join(format!(
            "matrix-test-config-use-{}-{}.json",
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let output = config_command(
            &mut matrix,
            ConfigCommand {
                command: ConfigSubcommand::Use {
                    profile: ConfigProfile::RedWiz,
                },
            },
        )
        .await
        .unwrap();

        assert_eq!(output["hasToken"], false);
        assert_eq!(output["tokenFile"], Value::Null);
        assert_eq!(output["hasTokenCommand"], true);
        assert!(matrix.config.token.is_none());
        assert!(matrix.config.token_file.is_none());
        assert_eq!(
            matrix.config.token_command.as_deref(),
            Some(RED_WIZ_TOKEN_COMMAND)
        );

        let saved: Config =
            serde_json::from_slice(&fs::read(&matrix.config_path).unwrap()).unwrap();
        assert!(saved.token.is_none());
        assert!(saved.token_file.is_none());
        assert_eq!(saved.token_command.as_deref(), Some(RED_WIZ_TOKEN_COMMAND));
        let _ = fs::remove_file(&matrix.config_path);
    }

    #[test]
    fn parses_update_install_path() {
        let cli = Cli::try_parse_from([
            "matrix",
            "update",
            "--check",
            "--install-path",
            "/home/me/bin/matrix",
        ])
        .expect("update install path parses");

        match cli.command {
            Commands::Update(UpdateCommand {
                check: true,
                install_path: Some(path),
                ..
            }) => {
                assert_eq!(path, PathBuf::from("/home/me/bin/matrix"));
            }
            _ => panic!("expected update command with install path"),
        }
    }

    #[test]
    fn page_values_accepts_facts_or_items_pages() {
        assert_eq!(
            page_values(&json!({"facts": [{"id": "fact-1"}]}), "facts")[0]["id"],
            "fact-1"
        );
        assert_eq!(
            page_values(&json!({"items": [{"id": "item-1"}]}), "facts")[0]["id"],
            "item-1"
        );
    }

    #[test]
    fn fact_cache_round_trips_metadata_and_facts() {
        let path = env::temp_dir().join(format!("matrix-cache-test-{}.sqlite", process::id()));
        let facts = vec![json!({
            "id": "fact-1",
            "zone": "runtime",
            "status": "passed",
            "subjectType": "service",
            "subjectName": "ledger-service"
        })];
        let db = Connection::open(&path).unwrap();
        populate_facts_table(&db, &facts).unwrap();
        write_fact_cache_metadata(
            &db,
            &FactCacheMetadata {
                construct: Some("https://matrix.example.test".to_string()),
                api_prefix: "/v1/compatibility".to_string(),
                profile: Some(ConfigProfile::RedWiz),
                schema_version: 2,
                fetched_at_unix: unix_now().unwrap(),
                checked_at_unix: Some(unix_now().unwrap()),
                fact_count: 1,
                max_facts: 1000,
                head_digest: Some("sha256:test-head".to_string()),
                head_fact_count: Some(1),
                head_latest_accepted_at: Some("2026-05-22T00:00:00.000Z".to_string()),
                head_latest_fact_id: Some("fact-1".to_string()),
                head_latest_content_hash: Some("sha256:test-fact".to_string()),
            },
        )
        .unwrap();
        drop(db);
        let db = open_fact_cache_db(&path, &MatrixContext::default(), None).unwrap();
        let metadata = read_fact_cache_metadata(&db).unwrap();
        let count: i64 = db
            .query_row("select count(*) from facts", [], |row| row.get(0))
            .unwrap();
        let zones: i64 = db
            .query_row("select count(*) from zones", [], |row| row.get(0))
            .unwrap();
        let persisted_views: i64 = db
            .query_row(
                "select count(*) from sqlite_master where type = 'view'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(metadata.schema_version, 2);
        assert_eq!(metadata.profile, Some(ConfigProfile::RedWiz));
        assert_eq!(metadata.fact_count, 1);
        assert_eq!(metadata.head_digest.as_deref(), Some("sha256:test-head"));
        assert_eq!(count, 1);
        assert_eq!(zones, 1);
        assert_eq!(persisted_views, 0);
    }

    #[test]
    fn human_bytes_formats_cache_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
    }

    #[test]
    fn cache_policy_parses_common_aliases() {
        assert_eq!(CachePolicy::parse("auto").unwrap(), CachePolicy::Auto);
        assert_eq!(
            CachePolicy::parse("prefer-cache").unwrap(),
            CachePolicy::PreferCache
        );
        assert_eq!(
            CachePolicy::parse("cache-first").unwrap(),
            CachePolicy::PreferCache
        );
        assert_eq!(CachePolicy::parse("refresh").unwrap(), CachePolicy::Refresh);
        assert_eq!(CachePolicy::parse("offline").unwrap(), CachePolicy::Offline);
    }

    #[test]
    fn matrix_uses_configured_cache_defaults() {
        let matrix = test_matrix(
            Config {
                cache_policy: Some(CachePolicy::PreferCache),
                cache_max_facts: Some(2500),
                ..Config::default()
            },
            Some(ConfigProfile::RedWiz),
        );
        assert_eq!(matrix.max_facts(None).unwrap(), 2500);
        assert_eq!(matrix.max_facts(Some(99)).unwrap(), 99);
        assert_eq!(matrix.cache_policy().unwrap(), CachePolicy::PreferCache);
        assert_eq!(
            matrix
                .fact_load_options(&FactCacheArgs::default())
                .unwrap()
                .policy,
            CachePolicy::PreferCache
        );
        assert_eq!(
            matrix
                .fact_load_options(&FactCacheArgs {
                    offline: true,
                    refresh_cache: false
                })
                .unwrap()
                .policy,
            CachePolicy::Offline
        );
    }

    #[test]
    fn cache_human_line_warns_when_local_cache_is_stale() {
        let cache = json!({
            "source": "cache",
            "factCount": 10,
            "ageHuman": "2d",
            "stale": true
        });
        let text = cache_human_line(cache.as_object().unwrap());
        assert!(text.contains("last refreshed 2d ago"));
        assert!(text.contains("Warning: using stale local Matrix cache"));
    }

    #[test]
    fn fact_cache_path_uses_sqlite_extension() {
        let matrix = test_matrix(Config::default(), Some(ConfigProfile::RedWiz));
        let path = fact_cache_path(&matrix).unwrap();
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("sqlite")
        );
    }

    #[test]
    fn query_json_keeps_cache_metadata_when_present() {
        let value = json!({
            "columns": ["id"],
            "rows": [{"id": "fact-1"}],
            "cache": {"source": "cache", "factCount": 1}
        });
        let normalized = normalize_query_result_value(&value);
        assert_eq!(normalized["rows"][0]["id"], "fact-1");
        assert_eq!(normalized["cache"]["source"], "cache");
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
    fn linux_manual_update_command_uses_release_archive() {
        let install_path = PathBuf::from("/usr/local/bin/matrix");
        let command = linux_manual_update_command("0.3.12", Some(&install_path));

        assert!(command.contains("gh release download v0.3.12"));
        assert!(command.contains("adrianmross/matrix"));
        assert!(command.contains("matrix-0.3.12-x86_64-unknown-linux-gnu.tar.gz"));
        assert!(command.ends_with(
            "sudo install /tmp/matrix-update/matrix-0.3.12-x86_64-unknown-linux-gnu/matrix /usr/local/bin/matrix"
        ));
    }

    #[test]
    fn linux_manual_update_command_uses_plain_install_for_user_path() {
        let command = linux_manual_update_command("0.3.12", Some(Path::new("/home/me/bin/matrix")));

        assert!(command.ends_with(
            "install /tmp/matrix-update/matrix-0.3.12-x86_64-unknown-linux-gnu/matrix /home/me/bin/matrix"
        ));
    }

    #[test]
    fn linux_manual_update_command_uses_plain_install_for_cargo_home_path() {
        let command = linux_manual_update_command("0.3.12", None);

        assert!(command.ends_with(
            "install /tmp/matrix-update/matrix-0.3.12-x86_64-unknown-linux-gnu/matrix ~/.cargo/bin/matrix"
        ));
    }

    #[test]
    fn direct_update_message_includes_source_and_archive_paths() {
        let message = direct_update_unavailable_message("0.3.12");

        assert!(message.contains("brew upgrade adrianmross/tap/matrix"));
        assert!(message.contains("gh release download v0.3.12"));
        assert!(message.contains(
            "cargo install --locked --git https://github.com/adrianmross/matrix --tag v0.3.12 matrix --force"
        ));
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
    fn producer_fact_batch_example_populates_query_views() {
        let value: Value =
            serde_json::from_str(include_str!("../examples/producers/fact-batch.json")).unwrap();
        let facts = value["facts"].as_array().unwrap().clone();
        let db = build_facts_db(&facts, &MatrixContext::default()).unwrap();

        let capabilities = execute_readonly_sql(
            &db,
            "select capability, capability_version, component from capabilities order by capability",
        )
        .unwrap();
        assert!(capabilities["rows"].as_array().unwrap().iter().any(|row| {
            row["capability"] == "http-api:example-api"
                && row["capability_version"] == "1"
                && row["component"] == "example-api"
        }));
        assert!(capabilities["rows"].as_array().unwrap().iter().any(|row| {
            row["capability"] == "sbom:example-api"
                && row["capability_version"] == "1.2.3"
                && row["component"] == "example-api"
        }));

        let requirements = execute_readonly_sql(
            &db,
            "select capability, capability_version, component from requirements order by capability",
        )
        .unwrap();
        assert_eq!(
            requirements["rows"][0],
            json!({
                "capability": "http-api:example-api",
                "capability_version": "1",
                "component": "example-worker"
            })
        );

        let members = execute_readonly_sql(
            &db,
            "select component, version, digest from members order by component",
        )
        .unwrap();
        assert_eq!(members["rows"][0]["component"], "example-api");
        assert_eq!(members["rows"][0]["version"], "1.2.3");
        assert!(
            members["rows"][0]["digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    fn graph_fixture_facts() -> Vec<Value> {
        vec![
            json!({
                "id": "aphrodite.1.2.0",
                "track": "odin",
                "status": "passed",
                "subject": {"type": "service", "name": "aphrodite", "version": "1.2.0", "repo": "red-wiz/aphrodite"},
                "requires": [
                    {"capability": "eos-platform", "version": "24.6"},
                    {"capability": "putto-client", "version": "0.8"}
                ]
            }),
            json!({
                "id": "eos.24.6.0",
                "track": "odin",
                "status": "passed",
                "subject": {"type": "release-bundle", "name": "eos", "version": "24.6.0", "repo": "red-wiz/eos"},
                "provides": [{"capability": "eos-platform", "version": "24.6"}],
                "members": [
                    {"component": "eunomia", "version": "3.1.0"},
                    {"component": "aglaea", "version": "5.0.0"},
                    {"component": "athena", "version": "2.4.0"}
                ]
            }),
            json!({
                "id": "eunomia.3.1.0",
                "track": "odin",
                "status": "passed",
                "subject": {"type": "service", "name": "eunomia", "version": "3.1.0", "repo": "red-wiz/eunomia"}
            }),
            json!({
                "id": "putto.0.8.1",
                "track": "odin",
                "status": "passed",
                "subject": {"type": "service", "name": "putto", "version": "0.8.1", "repo": "red-wiz/putto"},
                "provides": [{"capability": "putto-client", "version": "0.8"}]
            }),
        ]
    }

    fn graph_fixture_db() -> Connection {
        let facts = graph_fixture_facts();
        build_facts_db(&facts, &MatrixContext::default()).unwrap()
    }

    fn graph_fixture() -> GraphIndex {
        let db = graph_fixture_db();
        GraphIndex::from_db(&db).unwrap()
    }

    #[test]
    fn graph_finds_implicit_paths_through_release_bundles() {
        let graph = graph_fixture();

        let path = graph.path_answer("aphrodite", "eunomia", 5).unwrap();
        assert_eq!(path["found"], true);
        assert_eq!(path["paths"][0]["nodes"][0]["component"], "aphrodite");
        assert_eq!(path["paths"][0]["nodes"][1]["component"], "eos");
        assert_eq!(path["paths"][0]["nodes"][2]["component"], "eunomia");
        assert_eq!(path["paths"][0]["edges"][0]["relationship"], "requires");
        assert_eq!(path["paths"][0]["edges"][1]["relationship"], "contains");

        let siblings = graph.path_answer("aglaea", "athena", 5).unwrap();
        assert_eq!(siblings["found"], true);
        assert_eq!(
            siblings["paths"][0]["edges"][0]["relationship"],
            "member-of"
        );
        assert_eq!(siblings["paths"][0]["edges"][1]["relationship"], "contains");
    }

    #[test]
    fn graph_answers_works_with_and_versions_for() {
        let graph = graph_fixture();

        let works_with = graph.works_with_answer("putto", "aphrodite", 5).unwrap();
        assert_eq!(works_with["compatible"], true);
        assert_eq!(works_with["direction"], "right_to_left");
        assert_eq!(
            works_with["paths"][0]["edges"][0]["capability"],
            "putto-client"
        );

        let versions = graph
            .versions_for_answer("eunomia", "aphrodite", 5)
            .unwrap();
        assert_eq!(versions["versions"], json!(["3.1.0"]));
        assert_eq!(versions["versionCandidates"][0]["confidence"], "medium");
    }

    #[test]
    fn graph_paths_include_ranked_confidence() {
        let graph = graph_fixture();

        let path = graph.path_answer("aphrodite", "eunomia", 5).unwrap();
        assert_eq!(path["confidence"], "medium");
        assert_eq!(path["recommended"]["confidence"], "medium");
        assert!(path["paths"][0]["score"].as_i64().unwrap() > 0);
        assert!(
            path["paths"][0]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "inferred multi-hop path")
        );
    }

    #[test]
    fn native_graphql_binds_variables_and_projects_selection() {
        let db = graph_fixture_db();
        let graph = GraphIndex::from_db(&db).unwrap();
        let vars = parse_graphql_variables(&[
            "component=eunomia".to_string(),
            "for=aphrodite".to_string(),
        ])
        .unwrap();
        let value = execute_graphql_document(
            &db,
            &graph,
            "query Matrix($component:String!,$for:String!) {
                versions(component:$component, for:$for) {
                    versions
                    versionCandidates { version confidence score }
                }
            }",
            &vars,
            5,
        )
        .unwrap();

        assert_eq!(value["kind"], "graphql-result");
        assert_eq!(value["data"]["versions"]["versions"], json!(["3.1.0"]));
        assert_eq!(
            value["data"]["versions"]["versionCandidates"][0],
            json!({"version": "3.1.0", "confidence": "medium", "score": 119})
        );
        assert!(value["data"]["versions"].get("kind").is_none());
    }

    #[cfg(feature = "interactive")]
    #[test]
    fn parses_repl_graph_args_for_native_graphql() {
        let args = parse_repl_graph_args(vec![
            "--var",
            "component=eunomia",
            "--var=for=aphrodite",
            "--limit",
            "3",
            "query",
            "Matrix($component:String!,$for:String!)",
            "{",
            "versions(component:$component,",
            "for:$for)",
            "{",
            "versions",
            "}",
            "}",
        ])
        .unwrap();

        assert_eq!(
            args.query,
            Some(
                "query Matrix($component:String!,$for:String!) { versions(component:$component, for:$for) { versions } }"
                    .to_string()
            )
        );
        assert_eq!(args.vars, vec!["component=eunomia", "for=aphrodite"]);
        assert_eq!(args.limit, 3);
    }

    #[cfg(feature = "interactive")]
    #[test]
    fn repl_snippet_names_stay_inside_query_directory() {
        assert!(sanitize_repl_snippet_name("aphrodite-path").is_ok());
        assert!(sanitize_repl_snippet_name("../secret").is_err());
        assert!(sanitize_repl_snippet_name("nested/query").is_err());
        assert!(sanitize_repl_snippet_name("bad name").is_err());

        let graphql =
            repl_snippet_path("aphrodite-path", Some("{ path(from:\"a\", to:\"b\") }")).unwrap();
        assert_eq!(
            graphql.file_name().and_then(|value| value.to_str()),
            Some("aphrodite-path.graphql")
        );
        let shorthand = repl_snippet_path("short-path", Some("aphrodite -> eunomia")).unwrap();
        assert_eq!(
            shorthand.file_name().and_then(|value| value.to_str()),
            Some("short-path.graphql")
        );
        let sql = repl_snippet_path("current", Some("select * from current")).unwrap();
        assert_eq!(
            sql.file_name().and_then(|value| value.to_str()),
            Some("current.sql")
        );
    }

    #[test]
    fn native_graphql_supports_aliases_paths_and_producers() {
        let db = graph_fixture_db();
        let graph = GraphIndex::from_db(&db).unwrap();
        let value = execute_graphql_document(
            &db,
            &graph,
            "{
                aphroditePath: path(from:\"aphrodite\", to:\"eunomia\", limit:1) {
                    status
                    paths { confidence nodes { component version } }
                }
                producers(limit:10) { summary { producers facts } rows { producer facts freshness } }
            }",
            &BTreeMap::new(),
            5,
        )
        .unwrap();

        assert_eq!(value["data"]["aphroditePath"]["status"], "connected");
        assert_eq!(
            value["data"]["aphroditePath"]["paths"][0]["nodes"][1]["component"],
            "eos"
        );
        assert_eq!(value["data"]["producers"]["summary"]["facts"], 4);
        assert!(
            value["data"]["producers"]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["producer"] == "red-wiz/putto")
        );
    }

    #[test]
    fn native_graphql_reports_missing_variables_and_unknown_fields() {
        let db = graph_fixture_db();
        let graph = GraphIndex::from_db(&db).unwrap();
        let missing = execute_graphql_document(
            &db,
            &graph,
            "{ versions(component:$component, for:\"aphrodite\") { versions } }",
            &BTreeMap::new(),
            5,
        )
        .unwrap_err()
        .to_string();
        assert!(missing.contains("missing GraphQL variable $component"));

        let unsupported = execute_graphql_document(
            &db,
            &graph,
            "{ ask(question:\"no\") { answer } }",
            &BTreeMap::new(),
            5,
        )
        .unwrap_err()
        .to_string();
        assert!(unsupported.contains("unsupported GraphQL root field"));
    }

    #[test]
    fn matrix_graphql_schema_documents_root_fields() {
        assert!(MATRIX_GRAPHQL_SCHEMA.contains("type Query"));
        assert!(MATRIX_GRAPHQL_SCHEMA.contains("worksWith"));
        assert!(MATRIX_GRAPHQL_SCHEMA.contains("producers"));
    }

    #[test]
    fn producer_inventory_summarizes_fact_sources() {
        let facts = vec![
            json!({
                "id": "producer-a-1",
                "track": "odin",
                "status": "passed",
                "sourceRepository": "red-wiz/aphrodite",
                "observedAt": "2026-06-28T00:00:00Z",
                "subject": {"type": "service", "name": "aphrodite", "version": "1.2.0", "repo": "red-wiz/aphrodite"}
            }),
            json!({
                "id": "producer-a-2",
                "track": "odin",
                "status": "failed",
                "sourceRepository": "red-wiz/aphrodite",
                "observedAt": "2026-06-28T00:01:00Z",
                "subject": {"type": "service", "name": "eos", "version": "24.6.0", "repo": "red-wiz/eos"}
            }),
            json!({
                "id": "producer-b-1",
                "track": "odin",
                "status": "passed",
                "source": {"repo": "red-wiz/putto"},
                "observedAt": "2026-06-28T00:02:00Z",
                "subject": {"type": "service", "name": "putto", "version": "0.8.1", "repo": "red-wiz/putto"}
            }),
            json!({
                "id": "producer-c-1",
                "track": "runtime",
                "status": "passed",
                "observedAt": "2026-06-28T00:03:00Z",
                "subject": {"type": "service", "name": "athena", "version": "4.0.0", "repo": "red-wiz/athena"}
            }),
        ];
        let db = build_facts_db(&facts, &MatrixContext::default()).unwrap();
        let value = producer_inventory_value(&db, &MatrixContext::default(), 10, 14).unwrap();

        assert_eq!(value["kind"], "producer-inventory");
        assert_eq!(value["summary"]["producers"], 3);
        assert_eq!(value["summary"]["facts"], 4);
        assert_eq!(value["summary"]["sourceRepoFacts"], 3);
        assert_eq!(value["summary"]["inferredSubjectRepoFacts"], 1);
        assert_eq!(value["summary"]["missingProducerMetadataFacts"], 1);
        assert_eq!(value["rows"][0]["producer"], "red-wiz/athena");
        assert_eq!(
            value["rows"][0]["producer_metadata"],
            "inferred-subject-repo"
        );
        assert_eq!(value["rows"][2]["invalid_facts"], 1);

        let odin = producer_inventory_value(
            &db,
            &MatrixContext {
                zone: Some("odin".to_string()),
                ..MatrixContext::default()
            },
            10,
            14,
        )
        .unwrap();
        assert_eq!(odin["summary"]["producers"], 2);
        assert_eq!(odin["summary"]["facts"], 3);
        assert_eq!(odin["summary"]["missingProducerMetadataFacts"], 0);
    }

    #[test]
    fn graph_resolve_explains_alias_matches() {
        let graph = graph_fixture();

        let resolved = graph.resolve_answer("red-wiz/eunomia").unwrap();
        assert_eq!(resolved["kind"], "graph-resolve");
        assert_eq!(resolved["resolved"]["component"], "eunomia");
        assert_eq!(resolved["matches"][0]["aliasKinds"], json!(["repo"]));

        let text = graph_answer_human_text(resolved.as_object().unwrap());
        assert!(text.contains("Resolve: red-wiz/eunomia -> eunomia 3.1.0"));
        assert!(text.contains("through repo"));
    }

    #[test]
    fn parses_graphql_like_queries_for_agents() {
        match parse_graph_query("{ path(from:\"aphrodite\", to:\"eunomia\") { status paths { nodes { component version } } } }").unwrap() {
            GraphRequest::Path { source, target } => {
                assert_eq!(source, "aphrodite");
                assert_eq!(target, "eunomia");
            }
            _ => panic!("expected path request"),
        }

        match parse_graph_query(
            "query Matrix($ignored:String) { versions(component:\"putto\", for:\"aphrodite\") { versions } }",
        )
        .unwrap()
        {
            GraphRequest::VersionsFor {
                component,
                for_component,
            } => {
                assert_eq!(component, "putto");
                assert_eq!(for_component, "aphrodite");
            }
            _ => panic!("expected versions request"),
        }
    }

    #[test]
    fn renders_graph_answers_as_readable_human_text() {
        let graph = graph_fixture();
        let path = graph.path_answer("aphrodite", "eunomia", 5).unwrap();
        let text = graph_answer_human_text(path.as_object().unwrap());

        assert!(text.starts_with("Path: connected"));
        assert!(text.contains("aphrodite 1.2.0 -> eos 24.6.0 -> eunomia 3.1.0"));
        assert!(text.contains("aphrodite 1.2.0 requires eos 24.6.0 via eos-platform 24.6"));
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
