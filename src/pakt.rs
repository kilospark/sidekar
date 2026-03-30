use crate::*;
use regex::Regex;
use serde_json::Map;
use std::sync::LazyLock;

const PACK_MAGIC: &str = "@sidekar-pack 1";
const ALIAS_PREFIX: &str = "@k";

static ALIAS_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@k\d+$").expect("invalid alias regex"));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format {
    Json,
    Yaml,
    Csv,
    Packed,
}

impl Format {
    fn parse(name: &str) -> Result<Self> {
        match name {
            "json" => Ok(Self::Json),
            "yaml" | "yml" => Ok(Self::Yaml),
            "csv" => Ok(Self::Csv),
            "packed" | "pack" => Ok(Self::Packed),
            other => bail!("Unsupported format: {other}. Valid: json, yaml, csv"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Csv => "csv",
            Self::Packed => "packed",
        }
    }
}

struct PackedDocument {
    from: Format,
    keys: HashMap<String, String>,
    body: Value,
}

pub fn cmd_pack(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (path, from) = parse_pack_args(args)?;
    let input = read_input(path.as_deref())?;
    let format = match from {
        Some(format) => format,
        None => detect_format(path.as_deref(), &input)?,
    };
    if format == Format::Packed {
        bail!("Input is already packed. Use: sidekar unpack");
    }

    let value = parse_value(format, &input)?;
    let packed = pack_document(&value, format)?;
    write_output(ctx, &packed);
    Ok(())
}

pub fn cmd_unpack(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let (path, to) = parse_unpack_args(args)?;
    let input = read_input(path.as_deref())?;
    let packed = parse_packed_document(&input)?;
    let value = unpack_value(&packed.body, &packed.keys);
    let target = to.unwrap_or(packed.from);
    if target == Format::Packed {
        bail!("Use --to=json|yaml|csv when unpacking");
    }

    let rendered = render_value(&value, target)?;
    write_output(ctx, &rendered);
    Ok(())
}

fn parse_pack_args(args: &[String]) -> Result<(Option<String>, Option<Format>)> {
    let mut path = None;
    let mut from = None;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--from=") {
            from = Some(Format::parse(value)?);
        } else if arg == "-" {
            path = Some(arg.clone());
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if path.is_none() {
            path = Some(arg.clone());
        } else {
            bail!("Usage: sidekar pack [path|-] [--from=json|yaml|csv]");
        }
    }

    Ok((path, from))
}

fn parse_unpack_args(args: &[String]) -> Result<(Option<String>, Option<Format>)> {
    let mut path = None;
    let mut to = None;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--to=") {
            to = Some(Format::parse(value)?);
        } else if arg == "-" {
            path = Some(arg.clone());
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if path.is_none() {
            path = Some(arg.clone());
        } else {
            bail!("Usage: sidekar unpack [path|-] [--to=json|yaml|csv]");
        }
    }

    Ok((path, to))
}

fn read_input(path: Option<&str>) -> Result<String> {
    match path {
        Some("-") | None => {
            use std::io::Read;
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .context("failed to read stdin")?;
            Ok(input)
        }
        Some(path) => fs::read_to_string(path).with_context(|| format!("failed to read {path}")),
    }
}

fn write_output(ctx: &mut AppContext, content: &str) {
    ctx.output.push_str(content);
    if !content.ends_with('\n') {
        ctx.output.push('\n');
    }
}

fn detect_format(path: Option<&str>, input: &str) -> Result<Format> {
    let trimmed = input.trim_start();
    if trimmed.starts_with(PACK_MAGIC) {
        return Ok(Format::Packed);
    }

    if let Some(path) = path {
        if let Some(ext) = Path::new(path).extension().and_then(|ext| ext.to_str()) {
            if let Ok(format) = Format::parse(ext) {
                return Ok(format);
            }
        }
    }

    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Ok(Format::Json);
    }
    if serde_yaml::from_str::<Value>(trimmed).is_ok() {
        return Ok(Format::Yaml);
    }
    if looks_like_csv(trimmed) {
        return Ok(Format::Csv);
    }

    bail!("Could not detect input format. Use --from=json|yaml|csv");
}

fn looks_like_csv(input: &str) -> bool {
    let mut lines = input.lines().filter(|line| !line.trim().is_empty());
    let first = match lines.next() {
        Some(line) => line,
        None => return false,
    };
    let second = match lines.next() {
        Some(line) => line,
        None => return false,
    };
    first.contains(',') && second.contains(',')
}

fn parse_value(format: Format, input: &str) -> Result<Value> {
    match format {
        Format::Json => serde_json::from_str(input).context("failed to parse JSON"),
        Format::Yaml => serde_yaml::from_str(input).context("failed to parse YAML"),
        Format::Csv => parse_csv(input),
        Format::Packed => bail!("Packed input must be unpacked first"),
    }
}

fn parse_csv(input: &str) -> Result<Value> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(input.as_bytes());
    let headers = reader
        .headers()
        .context("failed to read CSV headers")?
        .clone();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.context("failed to read CSV record")?;
        let mut row = Map::new();
        for (header, value) in headers.iter().zip(record.iter()) {
            row.insert(header.to_string(), Value::String(value.to_string()));
        }
        rows.push(Value::Object(row));
    }
    Ok(Value::Array(rows))
}

fn render_value(value: &Value, format: Format) -> Result<String> {
    match format {
        Format::Json => serde_json::to_string_pretty(value).context("failed to render JSON"),
        Format::Yaml => serde_yaml::to_string(value).context("failed to render YAML"),
        Format::Csv => render_csv(value),
        Format::Packed => bail!("Cannot render packed output here"),
    }
}

fn render_csv(value: &Value) -> Result<String> {
    let rows = value
        .as_array()
        .context("CSV output requires a top-level array of objects")?;
    let mut headers = Vec::new();
    let mut object_rows = Vec::new();

    for row in rows {
        let object = row
            .as_object()
            .context("CSV output requires every row to be an object")?;
        for key in object.keys() {
            if !headers.iter().any(|header| header == key) {
                headers.push(key.clone());
            }
        }
        object_rows.push(object);
    }

    let mut writer = csv::WriterBuilder::new().from_writer(vec![]);
    writer
        .write_record(&headers)
        .context("failed to write CSV headers")?;
    for row in object_rows {
        let fields: Vec<String> = headers
            .iter()
            .map(|header| stringify_csv_value(row.get(header).unwrap_or(&Value::Null)))
            .collect();
        writer
            .write_record(&fields)
            .context("failed to write CSV row")?;
    }

    let bytes = writer
        .into_inner()
        .map_err(|err| anyhow!(err.into_error()))
        .context("failed to finalize CSV output")?;
    String::from_utf8(bytes).context("CSV output was not valid UTF-8")
}

fn stringify_csv_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn pack_document(value: &Value, from: Format) -> Result<String> {
    let aliases = build_aliases(value);
    let packed_body = pack_value(value, &aliases);
    let mut keys = Map::new();
    for (original, alias) in aliases {
        keys.insert(alias, Value::String(original));
    }

    let keys_json = serde_json::to_string(&Value::Object(keys)).context("failed to encode keys")?;
    let body_json = serde_json::to_string(&packed_body).context("failed to encode packed value")?;
    Ok(format!(
        "{PACK_MAGIC}\n@from {}\n@keys {keys_json}\n{body_json}",
        from.as_str()
    ))
}

fn build_aliases(value: &Value) -> HashMap<String, String> {
    let mut counts = HashMap::new();
    collect_key_counts(value, &mut counts);

    let mut keys: Vec<(String, usize, isize)> = counts
        .into_iter()
        .filter_map(|(key, count)| {
            let forced = ALIAS_KEY_RE.is_match(&key);
            if !forced && (count < 2 || key.len() <= 3) {
                return None;
            }
            let alias_len = format!("{ALIAS_PREFIX}{}", 0).len() as isize;
            let savings = (count as isize) * (key.len() as isize - alias_len);
            Some((key, count, savings))
        })
        .collect();
    keys.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });

    keys.into_iter()
        .enumerate()
        .map(|(idx, (key, _, _))| (key, format!("{ALIAS_PREFIX}{idx}")))
        .collect()
}

fn collect_key_counts(value: &Value, counts: &mut HashMap<String, usize>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                *counts.entry(key.clone()).or_insert(0) += 1;
                collect_key_counts(nested, counts);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_key_counts(item, counts);
            }
        }
        _ => {}
    }
}

fn pack_value(value: &Value, aliases: &HashMap<String, String>) -> Value {
    match value {
        Value::Object(map) => {
            let mut packed = Map::new();
            for (key, nested) in map {
                let alias = aliases.get(key).cloned().unwrap_or_else(|| key.clone());
                packed.insert(alias, pack_value(nested, aliases));
            }
            Value::Object(packed)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| pack_value(item, aliases)).collect())
        }
        _ => value.clone(),
    }
}

fn parse_packed_document(input: &str) -> Result<PackedDocument> {
    let trimmed = input.trim_start();
    let mut lines = trimmed.lines();
    let Some(first) = lines.next() else {
        bail!("Packed input is empty");
    };
    if first.trim() != PACK_MAGIC {
        bail!("Input is not in Sidekar packed format");
    }

    let mut from = None;
    let mut keys = HashMap::new();
    let mut body_lines = Vec::new();
    let mut in_body = false;

    for line in lines {
        if !in_body {
            if let Some(value) = line.strip_prefix("@from ") {
                from = Some(Format::parse(value.trim())?);
                continue;
            }
            if let Some(value) = line.strip_prefix("@keys ") {
                let parsed: HashMap<String, String> =
                    serde_json::from_str(value).context("failed to parse packed key map")?;
                keys = parsed;
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            in_body = true;
        }
        body_lines.push(line);
    }

    let from = from.context("Packed input missing @from header")?;
    if from == Format::Packed {
        bail!("Packed header cannot declare packed payload");
    }
    let body_text = body_lines.join("\n");
    let body = serde_json::from_str(&body_text).context("failed to parse packed body JSON")?;
    Ok(PackedDocument { from, keys, body })
}

fn unpack_value(value: &Value, keys: &HashMap<String, String>) -> Value {
    match value {
        Value::Object(map) => {
            let mut unpacked = Map::new();
            for (key, nested) in map {
                let original = keys.get(key).cloned().unwrap_or_else(|| key.clone());
                unpacked.insert(original, unpack_value(nested, keys));
            }
            Value::Object(unpacked)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| unpack_value(item, keys)).collect())
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_and_unpacks_json_with_repeated_keys() {
        let value = serde_json::json!({
            "users": [
                {"display_name": "Alice", "email_address": "alice@example.com"},
                {"display_name": "Bob", "email_address": "bob@example.com"}
            ]
        });

        let packed = pack_document(&value, Format::Json).expect("pack");
        assert!(packed.contains(PACK_MAGIC));
        assert!(packed.contains("@keys"));
        let body = packed.lines().last().expect("packed body");
        assert!(!body.contains("\"display_name\""));

        let parsed = parse_packed_document(&packed).expect("parse");
        let unpacked = unpack_value(&parsed.body, &parsed.keys);
        assert_eq!(unpacked, value);
    }

    #[test]
    fn csv_roundtrip_uses_array_of_objects() {
        let input = "name,email\nAlice,alice@example.com\nBob,bob@example.com\n";
        let value = parse_csv(input).expect("parse csv");
        let rendered = render_csv(&value).expect("render csv");
        let reparsed = parse_csv(&rendered).expect("reparse csv");
        assert_eq!(reparsed, value);
    }
}
