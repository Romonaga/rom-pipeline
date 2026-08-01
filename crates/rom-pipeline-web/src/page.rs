use std::fmt::Write;

use rom_pipeline_core::{
    AppConfig, GameCubeSettings, Nintendo3dsSettings, ProfileConfig, Ps2Settings, PspSettings,
    SystemKind, VitaSettings, WiiUSettings,
};
use rom_pipeline_service::ProfileStatus;

const STYLE: &str = r"
:root { color-scheme:dark; font-family:Inter,system-ui,sans-serif; background:#101419; color:#e9eef5 }
body { margin:0; min-height:100vh; background:radial-gradient(circle at top left,#233247 0,#101419 45%) }
main { max-width:1100px; margin:auto; padding:48px 24px }
h1 { font-size:clamp(2rem,5vw,4.5rem); margin:.1em 0; letter-spacing:-.05em }
.lede { color:#aab7c7; max-width:680px; font-size:1.1rem }
.profile-picker { display:flex; align-items:center; gap:12px; margin:28px 0 8px; color:#aab7c7 }
.profile-picker select { flex:1; color:#e9eef5; background:#0d1217; border:1px solid #34404d; border-radius:10px; padding:12px; font-size:1rem }
.card { display:none; background:#171d25; border:1px solid #2c3744; border-radius:20px; margin-top:18px; padding:24px; box-shadow:0 18px 60px #0005 }
.card.selected { display:block }
.heading,.actions { display:flex; align-items:center; justify-content:space-between; gap:18px; flex-wrap:wrap }
.eyebrow { color:#70d6ff; text-transform:uppercase; letter-spacing:.14em; font-size:.75rem; margin:0 }
h2 { margin:.2rem 0 }
.badge { border-radius:999px; padding:8px 13px; background:#34404d; text-transform:uppercase; font-size:.72rem; letter-spacing:.1em }
.badge.processing { background:#134e3a; color:#85f7c3 }.badge.waiting { background:#554317; color:#ffe28a }
.status-grid { display:grid; grid-template-columns:repeat(auto-fit,minmax(190px,1fr)); gap:12px; margin:22px 0 }
.status-grid div { background:#0f141a; border-radius:12px; padding:14px; min-width:0 }
.status-grid span,label { display:block; color:#8f9dae; font-size:.78rem; margin-bottom:6px }
.status-grid strong { display:block; overflow-wrap:anywhere }
.publication { background:#0f141a; border:1px solid #2c4957; border-radius:14px; padding:16px; margin:18px 0 }
.publication-heading { display:flex; justify-content:space-between; align-items:baseline; gap:12px; flex-wrap:wrap }
.publication-heading h3 { margin:0 }
.publication-heading strong { color:#85f7c3 }
.progress-track { height:12px; background:#26313d; border-radius:999px; overflow:hidden; margin:12px 0 14px }
.progress-fill { height:100%; background:linear-gradient(90deg,#38bdf8,#85f7c3); transition:width .3s ease }
.publication-grid { display:grid; grid-template-columns:repeat(auto-fit,minmax(150px,1fr)); gap:12px }
.publication-grid span { display:block; color:#8f9dae; font-size:.78rem; margin-bottom:5px }
.publication-grid strong { display:block; overflow-wrap:anywhere }
.prune-progress { border-color:#67414b }
.prune-progress .progress-fill { background:linear-gradient(90deg,#fb7185,#fbbf24) }
.prune-progress .publication-heading strong { color:#fbbf24 }
form { display:flex; align-items:end; gap:10px; flex-wrap:wrap }
input { color:#e9eef5; background:#0d1217; border:1px solid #34404d; border-radius:8px; padding:10px; min-width:80px }
.vita-title { flex:1 1 360px }.vita-title input { width:100%; box-sizing:border-box }.hint { color:#8f9dae; font-size:.78rem; margin:.3rem 0 0 }
button { border:0; border-radius:9px; padding:11px 16px; color:#07131b; background:#70d6ff; font-weight:700; cursor:pointer }
button.secondary { color:#e9eef5; background:#34404d }
.library-actions { margin-top:18px; padding-top:18px; border-top:1px solid #2c3744 }
button.danger { color:#fff; background:#9e3345 }
details { margin-top:24px; border-top:1px solid #2c3744; padding-top:18px }
.config { display:grid; grid-template-columns:repeat(auto-fit,minmax(280px,1fr)); gap:12px; margin-top:16px }
.config label { margin:0 }.config input { box-sizing:border-box; width:100%; margin-top:5px }
.save { margin-top:16px }
";

const SCRIPT: &str = r"
const cards = Array.from(document.querySelectorAll('[data-profile]'));
const picker = document.getElementById('profile-select');

async function refresh(card) {
    const id = card.dataset.profile;
    try {
      const response = await fetch('/api/status?profile=' + encodeURIComponent(id));
      if (!response.ok) return;
      const s = await response.json();
      const badge = card.querySelector('.badge');
      badge.textContent = s.activity;
      badge.className = 'badge ' + s.activity;
      document.getElementById('current-' + id).textContent = s.current;
      document.getElementById('groups-' + id).textContent = s.completed_groups + '/' + s.total_groups;
      document.getElementById('worker-' + id).textContent = s.active_worker || 'none';
      if (s.publication) {
        const p = s.publication;
        const percent = p.total ? Math.round((p.published * 100) / p.total) : 0;
        document.getElementById('publication-count-' + id).textContent =
          p.published + ' / ' + p.total + ' published (' + percent + '%)';
        document.getElementById('publication-bar-' + id).style.width = percent + '%';
        document.getElementById('publication-remaining-' + id).textContent = p.remaining;
        document.getElementById('publication-ready-' + id).textContent = p.ready;
        document.getElementById('publication-phase-' + id).textContent = p.phase;
        document.getElementById('publication-game-' + id).textContent = p.current_game || 'none';
        document.getElementById('publication-partial-' + id).textContent = p.partial_files;
      }
      if (s.prune) {
        const p = s.prune;
        const percent = p.total ? Math.round((p.removed * 100) / p.total) : 0;
        document.getElementById('prune-count-' + id).textContent =
          p.removed + ' / ' + p.total + ' source files removed (' + percent + '%)';
        document.getElementById('prune-bar-' + id).style.width = percent + '%';
        document.getElementById('prune-remaining-' + id).textContent = p.remaining;
        document.getElementById('prune-phase-' + id).textContent = p.phase;
        document.getElementById('prune-game-' + id).textContent = p.current_game || 'none';
      }
    } catch (_) {}
}

async function loadVitaJobs(card) {
  if (card.dataset.vita !== 'true' || card.dataset.jobsLoaded === 'true') return;
  const id = card.dataset.profile;
  const list = document.getElementById('jobs-' + id);
  const hint = document.getElementById('jobs-hint-' + id);
  try {
    const response = await fetch('/api/jobs?profile=' + encodeURIComponent(id));
    if (!response.ok) return;
    const jobs = await response.json();
    for (const job of jobs) {
      const option = document.createElement('option');
      option.value = job.id;
      option.label = job.name + ' [' + job.id.replace('VITA-', '') + ']';
      list.appendChild(option);
    }
    hint.textContent = jobs.length + ' archived games available; choose one game to deploy.';
    card.dataset.jobsLoaded = 'true';
  } catch (_) {
    hint.textContent = 'Unable to load the Vita title list.';
  }
}

function showProfile(id) {
  const selected = cards.find(card => card.dataset.profile === id) || cards[0];
  if (!selected) return;
  for (const card of cards) card.classList.toggle('selected', card === selected);
  picker.value = selected.dataset.profile;
  localStorage.setItem('rom-pipeline-profile', selected.dataset.profile);
  history.replaceState(null, '', '#' + encodeURIComponent(selected.dataset.profile));
  refresh(selected);
  loadVitaJobs(selected);
}

const hash = decodeURIComponent(location.hash.replace(/^#/, ''));
const remembered = localStorage.getItem('rom-pipeline-profile');
showProfile(hash || remembered || picker.value);
picker.addEventListener('change', () => showProfile(picker.value));
setInterval(() => {
  const selected = cards.find(card => card.classList.contains('selected'));
  if (selected) refresh(selected);
}, 3000);
";

pub fn render(config: &AppConfig, statuses: &[ProfileStatus]) -> String {
    let profiles = render_profiles(config, statuses);
    let selector = profile_selector(config, statuses);
    format!(
        r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>ROM Pipeline</title><style>{STYLE}</style>
</head><body><main>
<p class="eyebrow">Local conversion service</p>
<h1>ROM Pipeline</h1>
<p class="lede">Configure sources and outputs, process bounded batches, and see exactly what is active. Each system is implemented by its own adapter, including selective native deployment to SD2Vita.</p>
{selector}
{profiles}
</main><script>{SCRIPT}</script></body></html>"#
    )
}

fn profile_selector(config: &AppConfig, statuses: &[ProfileStatus]) -> String {
    let options =
        config
            .profiles
            .iter()
            .enumerate()
            .fold(String::new(), |mut options, (index, profile)| {
                let activity = statuses
                    .iter()
                    .find(|status| status.profile == profile.id)
                    .map_or("unknown", |status| status.activity.as_str());
                let _ = write!(
                    options,
                    "<option value=\"{}\"{}>{} - {}</option>",
                    escape(&profile.id),
                    if index == 0 { " selected" } else { "" },
                    escape(&profile.name),
                    escape(activity)
                );
                options
            });
    format!(
        "<label class=\"profile-picker\">System <select id=\"profile-select\">{options}</select></label>"
    )
}

fn render_profiles(config: &AppConfig, statuses: &[ProfileStatus]) -> String {
    let mut profiles = String::new();
    for (index, profile) in config.profiles.iter().enumerate() {
        let status = statuses.iter().find(|status| status.profile == profile.id);
        let activity = status.map_or("unknown", |status| status.activity.as_str());
        let current = status.map_or("not started", |status| status.current.as_str());
        let completed = status.map_or(0, |status| status.completed_groups);
        let total = status.map_or(0, |status| status.total_groups);
        let worker = status
            .and_then(|status| status.active_worker.as_deref())
            .unwrap_or("none");
        let publication = publication_status(profile, status);
        let prune = prune_status(profile, status);
        let _ = write!(
            profiles,
            r#"<section class="card{selected}" data-profile="{id}" data-vita="{vita}">
<div class="heading"><div><p class="eyebrow">{system}</p><h2>{name}</h2></div>
<span class="badge {activity}">{activity}</span></div>
<div class="status-grid">
<div><span>Current</span><strong id="current-{id}">{current}</strong></div>
<div><span>Completed</span><strong id="groups-{id}">{completed}/{total}</strong></div>
<div><span>Worker</span><strong id="worker-{id}">{worker}</strong></div></div>
{publication}
{prune}
{run_actions}
{library_actions}
{configuration}</section>"#,
            id = escape(&profile.id),
            system = escape(&format!("{:?}", profile.system)),
            name = escape(&profile.name),
            activity = escape(activity),
            selected = if index == 0 { " selected" } else { "" },
            vita = matches!(profile.system, SystemKind::PlayStationVita),
            current = escape(current),
            completed = completed,
            total = total,
            worker = escape(worker),
            run_actions = run_actions(profile),
            publication = publication,
            prune = prune,
            library_actions = library_actions(profile),
            configuration = configuration_form(profile),
        );
    }
    profiles
}

fn run_actions(profile: &ProfileConfig) -> String {
    let id = escape(&profile.id);
    if matches!(profile.system, SystemKind::PlayStationVita) {
        return format!(
            r#"<div class="actions"><form method="post" action="/profiles/start">
<input type="hidden" name="profile" value="{id}">
<label class="vita-title">Game to deploy
<input name="only" list="jobs-{id}" pattern="VITA-[A-Za-z0-9]{{9}}" required
 placeholder="Type a game name or Vita title ID, then select it" autocomplete="off"
 title="Select a game from the archived-game suggestions"></label>
<datalist id="jobs-{id}"></datalist>
<input name="limit" type="hidden" value="1">
<button type="submit">Deploy to SD2Vita</button>
<p class="hint" id="jobs-hint-{id}">Loading archived games...</p></form>
<form method="post" action="/profiles/stop"><input type="hidden" name="profile" value="{id}">
<button class="secondary" type="submit">Stop cleanly</button></form></div>"#
        );
    }
    format!(
        r#"<div class="actions"><form method="post" action="/profiles/start">
<input type="hidden" name="profile" value="{id}">
<label>Titles this run <input name="limit" type="number" min="1" value="{limit}"></label>
<button type="submit">Start / Resume</button></form>
<form method="post" action="/profiles/stop"><input type="hidden" name="profile" value="{id}">
<button class="secondary" type="submit">Stop cleanly</button></form></div>"#,
        limit = profile.batch_limit
    )
}

fn prune_status(profile: &ProfileConfig, status: Option<&ProfileStatus>) -> String {
    if !supports_library_actions(&profile.system) || profile.library_dir.is_none() {
        return String::new();
    }
    let Some(progress) = status.and_then(|status| status.prune.as_ref()) else {
        return String::new();
    };
    let percent = if progress.total == 0 {
        0
    } else {
        progress.removed.saturating_mul(100) / progress.total
    };
    format!(
        r#"<div class="publication prune-progress">
<div class="publication-heading"><h3>Source pruning progress</h3>
<strong id="prune-count-{id}">{removed} / {total} source files removed ({percent}%)</strong></div>
<div class="progress-track"><div class="progress-fill" id="prune-bar-{id}" style="width:{percent}%"></div></div>
<div class="publication-grid">
<div><span>Source files remaining</span><strong id="prune-remaining-{id}">{remaining}</strong></div>
<div><span>Phase</span><strong id="prune-phase-{id}">{phase}</strong></div>
<div><span>Current game</span><strong id="prune-game-{id}">{game}</strong></div>
</div></div>"#,
        id = escape(&profile.id),
        removed = progress.removed,
        total = progress.total,
        percent = percent,
        remaining = progress.remaining,
        phase = escape(&progress.phase),
        game = escape(progress.current_game.as_deref().unwrap_or("none")),
    )
}

fn publication_status(profile: &ProfileConfig, status: Option<&ProfileStatus>) -> String {
    if !supports_library_actions(&profile.system) || profile.library_dir.is_none() {
        return String::new();
    }
    let Some(progress) = status.and_then(|status| status.publication.as_ref()) else {
        return String::new();
    };
    let percent = if progress.total == 0 {
        0
    } else {
        progress.published.saturating_mul(100) / progress.total
    };
    format!(
        r#"<div class="publication">
<div class="publication-heading"><h3>Publication progress</h3>
<strong id="publication-count-{id}">{published} / {total} published ({percent}%)</strong></div>
<div class="progress-track"><div class="progress-fill" id="publication-bar-{id}" style="width:{percent}%"></div></div>
<div class="publication-grid">
<div><span>Remaining</span><strong id="publication-remaining-{id}">{remaining}</strong></div>
<div><span>Converted and ready</span><strong id="publication-ready-{id}">{ready}</strong></div>
<div><span>Phase</span><strong id="publication-phase-{id}">{phase}</strong></div>
<div><span>Current game</span><strong id="publication-game-{id}">{game}</strong></div>
<div><span>Temporary partials</span><strong id="publication-partial-{id}">{partial}</strong></div>
</div></div>"#,
        id = escape(&profile.id),
        published = progress.published,
        total = progress.total,
        percent = percent,
        remaining = progress.remaining,
        ready = progress.ready,
        phase = escape(&progress.phase),
        game = escape(progress.current_game.as_deref().unwrap_or("none")),
        partial = progress.partial_files,
    )
}

fn library_actions(profile: &ProfileConfig) -> String {
    if !supports_library_actions(&profile.system) || profile.library_dir.is_none() {
        return String::new();
    }
    format!(
        r#"<div class="actions library-actions">
<form method="post" action="/profiles/publish">
<input type="hidden" name="profile" value="{id}">
<label>Games to publish <input name="limit" type="number" min="1" value="{limit}"></label>
<button type="submit">Publish verified outputs</button></form>
<form method="post" action="/profiles/prune">
<input type="hidden" name="profile" value="{id}">
<label>Games to prune <input name="limit" type="number" min="1" value="{limit}"></label>
<label><input name="confirm" type="checkbox" value="yes" required> Permanently delete verified source files</label>
<button class="danger" type="submit">Prune source files</button></form></div>"#,
        id = escape(&profile.id),
        limit = profile.batch_limit,
    )
}

fn configuration_form(profile: &ProfileConfig) -> String {
    let fields = match profile.system {
        SystemKind::WiiU => {
            let Some(wiiu) = profile.wiiu.as_ref() else {
                return "<p>Wii U settings are missing.</p>".to_owned();
            };
            configuration_fields(profile, wiiu)
        }
        SystemKind::GameCube => {
            let Some(gamecube) = profile.gamecube.as_ref() else {
                return "<p>GameCube settings are missing.</p>".to_owned();
            };
            gamecube_configuration_fields(profile, gamecube)
        }
        SystemKind::Nintendo3ds => {
            let Some(settings) = profile.nintendo_3ds.as_ref() else {
                return "<p>Nintendo 3DS settings are missing.</p>".to_owned();
            };
            nintendo_3ds_configuration_fields(profile, settings)
        }
        SystemKind::PlayStationPortable => {
            let Some(psp) = profile.psp.as_ref() else {
                return "<p>PSP settings are missing.</p>".to_owned();
            };
            psp_configuration_fields(profile, psp)
        }
        SystemKind::PlayStation2 => {
            let Some(ps2) = profile.ps2.as_ref() else {
                return "<p>PS2 settings are missing.</p>".to_owned();
            };
            ps2_configuration_fields(profile, ps2)
        }
        SystemKind::PlayStationVita => {
            let Some(vita) = profile.vita.as_ref() else {
                return "<p>PlayStation Vita settings are missing.</p>".to_owned();
            };
            vita_configuration_fields(profile, vita)
        }
    };
    format!(
        r#"<details><summary>Configuration</summary>
<form class="config-form" method="post" action="/profiles/save">
<input type="hidden" name="profile" value="{id}">
<div class="config">{fields}</div>
<button class="save" type="submit">Save configuration</button>
</form></details>"#,
        id = escape(&profile.id),
        fields = fields,
    )
}

fn vita_configuration_fields(profile: &ProfileConfig, vita: &VitaSettings) -> String {
    let mut fields = vec![common_configuration_fields(profile)];
    fields.extend([
        path_field("7-Zip", "seven_zip", &vita.seven_zip),
        path_field("Mountpoint checker", "mountpoint", &vita.mountpoint),
        field(
            "Reserved free bytes on SD2Vita",
            "reserve_bytes",
            &vita.reserve_bytes.to_string(),
            "number",
        ),
    ]);
    fields.join("")
}

fn nintendo_3ds_configuration_fields(
    profile: &ProfileConfig,
    settings: &Nintendo3dsSettings,
) -> String {
    let mut fields = vec![common_configuration_fields(profile)];
    fields.extend([
        path_field("Download manifest", "manifest", &settings.manifest),
        path_field("7-Zip", "seven_zip", &settings.seven_zip),
        path_field("Python", "python", &settings.python),
        path_field("CCI-to-CIA converter", "converter", &settings.converter),
        path_field("CTRTool", "ctrtool", &settings.ctrtool),
    ]);
    fields.join("")
}

fn gamecube_configuration_fields(profile: &ProfileConfig, gamecube: &GameCubeSettings) -> String {
    let mut fields = vec![common_configuration_fields(profile)];
    fields.extend([
        path_field("Download manifest", "manifest", &gamecube.manifest),
        path_field("Dolphin Tool", "dolphin_tool", &gamecube.dolphin_tool),
        field(
            "RVZ block size",
            "block_size",
            &gamecube.block_size.to_string(),
            "number",
        ),
        field(
            "RVZ compression",
            "compression",
            &gamecube.compression,
            "text",
        ),
        field(
            "RVZ compression level",
            "compression_level",
            &gamecube.compression_level.to_string(),
            "number",
        ),
        field(
            "Full round-trip verification",
            "verify_round_trip",
            &gamecube.verify_round_trip.to_string(),
            "text",
        ),
    ]);
    fields.join("")
}

fn ps2_configuration_fields(profile: &ProfileConfig, ps2: &Ps2Settings) -> String {
    let mut fields = vec![common_configuration_fields(profile)];
    fields.extend([
        path_field("Download manifest", "manifest", &ps2.manifest),
        path_field("CHDMan", "chdman", &ps2.chdman),
        field(
            "Minimum savings percent",
            "minimum_savings_percent",
            &ps2.minimum_savings_percent.to_string(),
            "number",
        ),
        field(
            "Preserve original when compression is not worthwhile",
            "preserve_when_compression_is_not_worthwhile",
            &ps2.preserve_when_compression_is_not_worthwhile.to_string(),
            "text",
        ),
        field(
            "Full round-trip verification",
            "verify_round_trip",
            &ps2.verify_round_trip.to_string(),
            "text",
        ),
    ]);
    fields.join("")
}

fn supports_library_actions(system: &SystemKind) -> bool {
    matches!(
        system,
        SystemKind::GameCube
            | SystemKind::Nintendo3ds
            | SystemKind::PlayStationPortable
            | SystemKind::PlayStation2
    )
}

fn psp_configuration_fields(profile: &ProfileConfig, psp: &PspSettings) -> String {
    let mut fields = vec![common_configuration_fields(profile)];
    fields.extend([
        path_field("CHDMan", "chdman", &psp.chdman),
        field("Compression codec", "codec", &psp.codec, "text"),
        field(
            "Hunk size",
            "hunk_size",
            &psp.hunk_size.to_string(),
            "number",
        ),
        field(
            "Full round-trip verification",
            "verify_round_trip",
            &psp.verify_round_trip.to_string(),
            "text",
        ),
    ]);
    fields.join("")
}

fn configuration_fields(profile: &ProfileConfig, wiiu: &WiiUSettings) -> String {
    let mut fields = vec![common_configuration_fields(profile)];
    fields.extend([
        path_field("Manifest", "manifest", &wiiu.manifest),
        path_field("CDecrypt", "cdecrypt", &wiiu.cdecrypt),
        path_field("ZArchive", "zarchive", &wiiu.zarchive),
        field(
            "Source wait seconds",
            "wait_seconds",
            &wiiu.wait_seconds.to_string(),
            "number",
        ),
    ]);
    fields.join("")
}

fn common_configuration_fields(profile: &ProfileConfig) -> String {
    [
        field("Display name", "name", &profile.name, "text"),
        field(
            "Source contents",
            "source_format",
            &profile.source_format,
            "text",
        ),
        path_field("Source folder", "source_dir", &profile.source_dir),
        path_field("Done folder", "done_dir", &profile.done_dir),
        path_field("Work folder", "work_dir", &profile.work_dir),
        path_field("State folder", "state_dir", &profile.state_dir),
        path_field("Log folder", "log_dir", &profile.log_dir),
        path_field("Output folder", "output_dir", &profile.output_dir),
        profile
            .library_dir
            .as_ref()
            .map_or_else(String::new, |path| {
                path_field("Final library folder", "library_dir", path)
            }),
        field(
            "Output format",
            "output_format",
            &profile.output_format,
            "text",
        ),
        field(
            "Default batch",
            "batch_limit",
            &profile.batch_limit.to_string(),
            "number",
        ),
    ]
    .join("")
}

fn path_field(label: &str, name: &str, path: &std::path::Path) -> String {
    field(label, name, &path.display().to_string(), "text")
}

fn field(label: &str, name: &str, value: &str, kind: &str) -> String {
    format!(
        r#"<label>{}<input type="{}" name="{}" value="{}" required></label>"#,
        escape(label),
        escape(kind),
        escape(name),
        escape(value)
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
