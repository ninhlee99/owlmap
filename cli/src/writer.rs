//! Turns a [`RunResult`] into a folder of Markdown files.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::analyzer::RunResult;
use crate::i18n::Labels;
use crate::client::Usage;
use crate::config::Config;

/// What was mapped, for the index and metadata.
pub struct SourceInfo {
    pub name: String,
    pub url: Option<String>,
    pub commit: Option<String>,
}

pub fn write(out: &Path, result: &RunResult, source: &SourceInfo, config: &Config, usage: Option<Usage>) -> Result<PathBuf> {
    fs::create_dir_all(out.join("modules"))?;
    for (name, text) in &result.documents {
        fs::write(out.join(name), text)?;
    }
    let labels = config.lang.labels();
    // Remove notes for modules that no longer exist, so the folder matches this run.
    if let Ok(entries) = fs::read_dir(out.join("modules")) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "md") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    for s in &result.summaries {
        fs::write(out.join("modules").join(format!("{}.md", str_of(s, "slug"))), module_markdown(s, labels))?;
    }
    fs::write(out.join("README.md"), index_markdown(result, source, labels))?;
    fs::write(out.join("owlmap.json"), serde_json::to_string_pretty(&metadata(result, source, config, usage))?)?;
    Ok(out.to_path_buf())
}

pub fn module_markdown(s: &Map<String, Value>, l: &Labels) -> String {
    let mut out = format!("# {}\n\n", str_of(s, "module"));
    if let Some(e) = s.get("error").and_then(Value::as_str) {
        out += &format!("{}: {e}\n\n", l.module_failed);
    } else if s.contains_key("parse_error") {
        out += &format!("{}\n\n", str_of(s, "notes"));
    } else {
        let purpose = str_of(s, "purpose");
        if !purpose.trim().is_empty() {
            out += &format!("{purpose}\n\n");
        }
        let key_files: Vec<String> = list(s, "key_files")
            .iter()
            .filter_map(|k| {
                let path = k.get("path")?.as_str()?;
                Some(format!("`{path}` — {}", k.get("role").and_then(Value::as_str).unwrap_or("")))
            })
            .collect();
        out += &section(l.key_files, &key_files);
        for (title, key) in [
            (l.public_interface, "public_interface"),
            (l.depends_on, "depends_on"),
            (l.used_by, "used_by"),
            (l.data, "data"),
            (l.risks, "risks"),
        ] {
            out += &section(title, &strings(s, key));
        }
        let notes = str_of(s, "notes");
        if !notes.trim().is_empty() {
            out += &format!("## {}\n\n{notes}\n\n", l.notes);
        }
    }
    let files = strings(s, "files");
    out += &format!("<details><summary>{} ({})</summary>\n\n", l.all_files, files.len());
    out += &files.iter().map(|f| format!("- `{f}`")).collect::<Vec<_>>().join("\n");
    out += "\n\n</details>\n";
    out
}

fn index_markdown(result: &RunResult, source: &SourceInfo, l: &Labels) -> String {
    let plan = &result.plan;
    let mut langs: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &plan.files {
        *langs.entry(f.language.as_str()).or_default() += 1;
    }
    let mut langs: Vec<_> = langs.into_iter().collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let langs = langs.iter().take(6).map(|(l, n)| format!("{l} {n}")).collect::<Vec<_>>().join(", ");

    let rows = result
        .summaries
        .iter()
        .map(|s| {
            let purpose = if s.contains_key("error") { l.summary_failed.to_string() } else { first_sentence(&str_of(s, "purpose")) };
            format!(
                "| [{}](modules/{}.md) | {} | {} |",
                str_of(s, "module"),
                str_of(s, "slug"),
                strings(s, "files").len(),
                purpose
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let areas = if result.areas.is_empty() {
        String::new()
    } else {
        let rows = result
            .areas
            .iter()
            .map(|a| {
                let n = a.get("modules").and_then(Value::as_array).map_or(0, Vec::len);
                format!("| {} | {} | {} |", str_of(a, "area"), n, first_sentence(&str_of(a, "purpose")))
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("## {}\n\n| {} | {} | {} |\n|---|---|---|\n{rows}\n\n", l.areas, l.area, l.modules, l.purpose)
    };
    let what = match &source.url {
        Some(u) => format!("[{u}]({u})"),
        None => format!("`{}`", source.name),
    };
    let at = source.commit.as_ref().map(|c| format!(" {} `{}`", l.at_commit, &c[..c.len().min(12)])).unwrap_or_default();

    format!(
        "# {name} — OwlMap\n\n{gen} {what}{at}.\n\n\
| {doc} | {ans} |\n|---|---|\n\
| [{a0}](ARCHITECTURE.md) | {a1} |\n\
| [{f0}](FLOWS.md) | {f1} |\n\
| [{o0}](ONBOARDING.md) | {o1} |\n\n\
**{nf} {files_in} {nm} {mods_lc}** · {langs}\n\n{areas}## {mods}\n\n| {module} | {files} | {purpose} |\n|---|---|---|\n{rows}\n\n\
---\n{footer}\n",
        name = source.name,
        gen = l.generated_for,
        doc = l.document,
        ans = l.answers,
        a0 = l.architecture.0,
        a1 = l.architecture.1,
        f0 = l.flows.0,
        f1 = l.flows.1,
        o0 = l.onboarding.0,
        o1 = l.onboarding.1,
        nf = plan.files.len(),
        files_in = l.files_in,
        nm = plan.modules.len(),
        mods_lc = l.modules.to_lowercase(),
        mods = l.modules,
        module = l.module,
        files = l.files,
        purpose = l.purpose,
        footer = l.footer,
    )
}

fn metadata(result: &RunResult, source: &SourceInfo, config: &Config, usage: Option<Usage>) -> Value {
    json!({
        "owlmap_version": crate::VERSION,
        "generated_at": now_rfc3339(),
        "source": { "name": source.name, "url": source.url, "commit": source.commit },
        "models": { "fast": config.fast_model, "smart": config.smart_model },
        "lang": config.lang.code(),
        "reused": { "modules": result.reused_modules, "documents": result.reused_documents },
        "areas": result.areas.iter().map(|a| json!({ "area": a.get("area"), "modules": a.get("modules") })).collect::<Vec<_>>(),
        "files": result.plan.files.len(),
        "modules": result.plan.modules.len(),
        "skipped": result.plan.skipped,
        "estimated_input_tokens": result.plan.estimated_input_tokens,
        "usage": usage.map(|u| u.to_json()),
        "failed_modules": result.failures,
    })
}

fn section(title: &str, items: &[String]) -> String {
    let items: Vec<&String> = items.iter().filter(|i| !i.trim().is_empty()).collect();
    if items.is_empty() {
        return String::new();
    }
    format!("## {title}\n\n{}\n\n", items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n"))
}

fn str_of(s: &Map<String, Value>, key: &str) -> String {
    match s.get(key) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn list<'a>(s: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    s.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn strings(s: &Map<String, Value>, key: &str) -> Vec<String> {
    list(s, key)
        .iter()
        .map(|v| match v {
            Value::String(x) => x.clone(),
            other => other.to_string(),
        })
        .collect()
}

fn first_sentence(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "\\|");
    let bytes = t.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            return t[..=i].to_string();
        }
    }
    t
}

/// UTC timestamp without pulling in a date crate.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}
