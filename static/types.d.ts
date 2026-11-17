// OliveTin-RS 前后端契约类型（零构建渐进类型化，阶段 1）。
//
// 这不是构建产物：app.js 仍由 Rust 的 include_str! 原样内联，
// 本文件只服务于 tsc --noEmit / IDE 与 verify_contract.py 的字段比对。
//
// 后端响应由 handler.rs 的 117 处 json! 宏构造，没有 #[derive(Serialize)]
// 结构体可供生成类型，因此这里的字段是**手写契约**。
// 漂移检测由 target/verify-patch/verify_contract.py 完成：它会把
// config.rs 的 struct 字段与下面的 interface 做集合比对。

// ---------- 配置实体（对应 src/config.rs） ----------

interface VersioningConfig {
  enabled: boolean;
  base_dir: string;
  binary_name: string | null;
  config_name: string | null;
  config_files: string[];
  upload: UploadConfig;
}

interface UploadConfig {
  enabled: boolean;
  max_binary_mb: number;
  max_config_kb: number;
}

interface ScheduleConfig {
  enabled: boolean;
  mode: 'daily' | 'weekly' | 'monthly' | 'cron';
  timezone: string | null;
  skip_if_running: boolean;
  time: string | null;
  weekdays: number[] | null;
  days: number[] | null;
  cron: string | null;
}

interface ParamDef {
  name: string;
  description: string;
  required: boolean;
  default: string | null;
  type: string;
  style: string;
  options: string[];
}

interface SubcommandDef {
  name: string;
  description: string;
  params: ParamDef[];
}

interface DiscoverConfig {
  enabled: boolean;
  command: string[] | null;
  parser: string;
  timeout: number;
}

/** src/config.rs:485 —— 24 个字段，与 verify_contract.py 的比对源一一对应。 */
interface Action {
  id: string;
  title: string;
  enabled: boolean;
  group: string;
  exec: string[];
  command: string | null;
  script_name: string | null;
  script_full_path: string | null;
  timeout: number;
  concurrency: number;
  description: string | null;
  push: boolean;
  push_condition: string;
  push_title: string | null;
  push_content: string | null;
  push_uids: string[] | null;
  push_topic_ids: number[] | null;
  push_content_type: number | null;
  env: Record<string, string>;
  versioning: VersioningConfig | null;
  schedule: ScheduleConfig | null;
  args: ParamDef[];
  discover: DiscoverConfig | null;
  subcommands: SubcommandDef[];
}

/** src/config.rs:1360 —— 设置页 POST /api/settings 的 body（15 个可选字段）。 */
interface SettingsUpdate {
  log_retention_days?: number;
  log_max_total_mb?: number;
  versioning_enabled?: boolean;
  push_enabled?: boolean;
  push_spt?: string;
  push_app_token?: string;
  push_uids?: string;
  push_include_output?: boolean;
  max_versions?: number;
  auth_enabled?: boolean;
  collapse_groups_by_default?: boolean;
  group_layout?: string;
  group_panel_position?: string;
  group_order?: string[];
  push_summary_mode?: 'auto' | 'content' | 'none';
}

// ---------- API 响应（对应 src/handler.rs 的 json!） ----------

interface SettingsResponse {
  log_directory: string | null;
  script_directory: string;
  log_retention_days: number | null;
  log_max_total_mb: number;
  log_usage_bytes: number;
  log_usage_files: number;
  log_usage_rule: string;
  min_free_mb: number;
  max_versions: number;
  versioning_enabled: boolean;
  max_versions_min: number;
  max_versions_max: number;
  auth_enabled: boolean;
  collapse_groups_by_default: boolean;
  group_layout: string;
  group_panel_position: string;
  group_order: string[];
  push: {
    enabled: boolean;
    spt: string;
    spt_configured: boolean;
    app_token: string;
    app_token_configured: boolean;
    uids: string;
    include_output: boolean;
    summary_mode: 'auto' | 'content' | 'none';
  };
}

/** get_settings / list_actions 展示层的 schedule 对象（8 字段）。 */
interface ScheduleDisplay {
  enabled: boolean;
  mode: string;
  description: string;
  timezone: string;
  next_run_at: string | null;
  next_run_local: string | null;
  next_run_schedule_tz: string | null;
  next_run_human: string | null;
}

/** list_actions 每个 action 的展示对象（handler.rs:941-960，17 字段）。 */
interface ActionListItem {
  id: string;
  title: string;
  enabled: boolean;
  group: string;
  description: string | null;
  state: 'running' | 'idle';
  concurrency: number;
  active_version: string | null;
  upload_enabled: boolean;
  params: ParamDef[];
  subcommands: SubcommandDef[];
  args: ParamDef[];
  discover: DiscoverConfig | null;
  schedule: ScheduleDisplay | null;
  has_script: boolean;
  script_path: string | null;
  script_ref: string | null;
}

interface ActionsResponse {
  groups: Array<{ name: string; actions: ActionListItem[] }>;
  queued_total: number;
}

interface LogEntry {
  id: number;
  action_id: string;
  action_title: string;
  action_group: string;
  action_version: string | null;
  started_at: string;
  started_at_local: string | null;
  duration_ms: number;
  exit_code: number | null;
  stdout: string;
  stderr: string;
  truncated: boolean;
  timed_out: boolean;
  trigger: 'manual' | 'schedule';
  push_status: 'ok' | 'disabled' | { failed: string } | null;
  kind: 'run' | 'create' | 'update' | 'delete';
  script_path: string | null;
}

interface LogsResponse {
  items: LogEntry[];
  has_more: boolean;
  next_before_id: number | null;
}

interface VersionInfo {
  /** 与页面顶栏徽标一致，形如 `v0.2.0`（含 v 前缀）。 */
  version: string;
  git_commit: string;
  built_at: string;
  rustc: string;
}
