//! Search across workspace files (regex or literal), like Remix's Search plugin.

use std::path::Path;

pub struct SearchHit {
    pub file: String,
    pub line_no: usize,
    pub line: String,
}

pub fn search(root: &Path, pattern: &str, regex_mode: bool) -> Result<Vec<SearchHit>, String> {
    let re = if regex_mode {
        Some(regex::Regex::new(pattern).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let needle = if regex_mode { None } else { Some(pattern.to_string()) };
    let mut hits = vec![];
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().map(|e| e == "sol" || e == "md" || e == "json" || e == "txt" || e == "rrx").unwrap_or(false)
        {
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
            for (i, line) in content.lines().enumerate() {
                let matched = match (&re, &needle) {
                    (Some(re), _) => re.is_match(line),
                    (_, Some(n)) => line.contains(n.as_str()),
                    _ => false,
                };
                if matched {
                    hits.push(SearchHit {
                        file: rel.clone(),
                        line_no: i + 1,
                        line: line.trim().to_string(),
                    });
                }
                if hits.len() >= 500 {
                    return Ok(hits);
                }
            }
        }
    }
    Ok(hits)
}
