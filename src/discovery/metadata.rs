//! Bounded reads of launcher metadata files and the two small parsers they
//! need: Valve's VDF text format and protobuf wire fields.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub fn parse_vdf(text: &str) -> Result<Value> {
    if text.len() > 16 * 1024 * 1024 {
        bail!("Launcher metadata exceeds 16 MiB");
    }
    // One manifest per installed Steam game is parsed on every refresh.
    static TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"//[^\n]*|"((?:\\.|[^"\\])*)"|([{}])"#).expect("constant token pattern")
    });
    let tokens: Vec<String> = TOKEN
        .captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .map(|m| {
                    format!(
                        "Q{}",
                        m.as_str().replace(r#"\""#, "\"").replace(r"\\", r"\")
                    )
                })
                .or_else(|| c.get(2).map(|m| m.as_str().into()))
        })
        .collect();
    fn object(
        tokens: &[String],
        position: &mut usize,
        nested: bool,
        depth: usize,
    ) -> Result<Value> {
        if depth > 64 {
            bail!("KeyValues nesting exceeds 64 levels");
        }
        let mut map = Map::new();
        while *position < tokens.len() {
            let key = &tokens[*position];
            *position += 1;
            if key == "}" {
                if nested {
                    return Ok(Value::Object(map));
                }
                bail!("Unexpected KeyValues closing brace");
            }
            let key = key
                .strip_prefix('Q')
                .context("Expected KeyValues key")?
                .to_owned();
            let token = tokens.get(*position).context("Incomplete KeyValues")?;
            *position += 1;
            let value = if token == "{" {
                object(tokens, position, true, depth + 1)?
            } else {
                Value::String(
                    token
                        .strip_prefix('Q')
                        .context("Expected KeyValues value")?
                        .into(),
                )
            };
            map.insert(key, value);
        }
        if nested {
            bail!("Incomplete KeyValues object");
        }
        Ok(Value::Object(map))
    }
    object(&tokens, &mut 0, false, 0)
}
pub(super) fn entries(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok().map(|e| e.path()))
        .collect()
}
pub(super) fn read_metadata(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("Launcher metadata exceeds 16 MiB");
    }
    Ok(bytes)
}
pub(super) fn read_metadata_text(path: impl AsRef<Path>) -> Result<String> {
    Ok(String::from_utf8(read_metadata(path)?)?)
}
pub(super) fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        read_metadata_text(path)?.trim_start_matches('\u{feff}'),
    )?)
}
pub fn protobuf_fields(mut bytes: &[u8]) -> Result<Vec<(u64, &[u8])>> {
    fn varint(bytes: &mut &[u8]) -> Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let b = *bytes.first().context("Truncated protobuf")?;
            *bytes = &bytes[1..];
            if shift == 63 && b > 1 {
                bail!("Protobuf integer overflow");
            }
            value |= ((b & 127) as u64) << shift;
            if b & 128 == 0 {
                return Ok(value);
            }
        }
        bail!("Invalid protobuf integer")
    }
    let mut result = vec![];
    while !bytes.is_empty() {
        let tag = varint(&mut bytes)?;
        let (field, wire) = (tag >> 3, tag & 7);
        if field == 0 {
            bail!("Invalid protobuf field");
        }
        let length = match wire {
            0 => {
                varint(&mut bytes)?;
                continue;
            }
            1 => 8,
            2 => usize::try_from(varint(&mut bytes)?)?,
            5 => 4,
            _ => bail!("Unsupported protobuf wire type"),
        };
        if length > bytes.len() {
            bail!("Truncated protobuf field");
        }
        if wire == 2 {
            result.push((field, &bytes[..length]));
        }
        bytes = &bytes[length..];
    }
    Ok(result)
}
