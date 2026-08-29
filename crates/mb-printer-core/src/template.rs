// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeMap;
use thiserror::Error;
#[derive(Debug, Error, PartialEq)]
pub enum TemplateError {
    #[error("unclosed expression")]
    Unclosed,
    #[error("unknown field: {0}")]
    Field(String),
    #[error("unknown transform: {0}")]
    Transform(String),
    #[error("template limit exceeded")]
    Limit,
    #[error("invalid numeric/date value: {0}")]
    Value(String),
}
pub struct Context<'a> {
    pub fields: &'a BTreeMap<String, String>,
    pub locale: &'a str,
    pub current_date: &'a str,
}
pub fn evaluate(input: &str, fields: &BTreeMap<String, String>) -> Result<String, TemplateError> {
    evaluate_with_context(
        input,
        Context {
            fields,
            locale: "en",
            current_date: "1970-01-01",
        },
    )
}
pub fn evaluate_with_context(input: &str, context: Context<'_>) -> Result<String, TemplateError> {
    if input.len() > 1_000_000 {
        return Err(TemplateError::Limit);
    };
    let mut out = String::new();
    let mut rest = input;
    while let Some(a) = rest.find("{{") {
        out.push_str(&rest[..a]);
        let after = &rest[a + 2..];
        let b = after.find("}}").ok_or(TemplateError::Unclosed)?;
        let parts: Vec<_> = after[..b].split('|').map(str::trim).collect();
        let mut value = if parts[0] == "@date" {
            context.current_date.to_owned()
        } else {
            match context.fields.get(parts[0]) {
                Some(value) => value.clone(),
                None if parts[1..]
                    .iter()
                    .any(|part| part.starts_with("default:") || part.starts_with("if-empty:")) =>
                {
                    String::new()
                }
                None => return Err(TemplateError::Field(parts[0].into())),
            }
        };
        for f in &parts[1..] {
            value = transform(f, &value, &context)?
        }
        out.push_str(&value);
        if out.len() > 1_000_000 {
            return Err(TemplateError::Limit);
        }
        rest = &after[b + 2..]
    }
    out.push_str(rest);
    Ok(out)
}
fn transform(f: &str, v: &str, context: &Context<'_>) -> Result<String, TemplateError> {
    Ok(match f {
        "upper" => v.to_uppercase(),
        "lower" => v.to_lowercase(),
        "trim" => v.trim().into(),
        "ascii" => v.chars().filter(|c| c.is_ascii()).collect(),
        _ if f.starts_with("default:") && v.is_empty() => f[8..].trim_matches('"').into(),
        _ if f.starts_with("default:") => v.into(),
        _ if f.starts_with("prefix:") => format!("{}{}", f[7..].trim_matches('"'), v),
        _ if f.starts_with("suffix:") => format!("{}{}", v, f[7..].trim_matches('"')),
        _ if f.starts_with("number:") => {
            let decimals = f[7..]
                .parse::<u8>()
                .map_err(|_| TemplateError::Value(f.into()))?
                .min(9) as usize;
            let mut out = format_decimal(v, decimals)?;
            if context.locale.starts_with("fr") {
                out = out.replace('.', ",")
            }
            out
        }
        _ if f.starts_with("if-empty:") => {
            let p: Vec<_> = f[9..].splitn(2, ':').collect();
            if p.len() != 2 {
                return Err(TemplateError::Transform(f.into()));
            }
            if v.is_empty() {
                p[0].into()
            } else {
                p[1].into()
            }
        }
        _ if f.starts_with("if-eq:") => {
            let p: Vec<_> = f[6..].splitn(3, ':').collect();
            if p.len() != 3 {
                return Err(TemplateError::Transform(f.into()));
            }
            if v == p[0] { p[1].into() } else { p[2].into() }
        }
        _ if f.starts_with("date:") => format_date(v, &f[5..])?,
        _ => return Err(TemplateError::Transform(f.into())),
    })
}
fn format_date(value: &str, pattern: &str) -> Result<String, TemplateError> {
    let p: Vec<_> = value.split('-').collect();
    if p.len() != 3 || p[0].len() != 4 || p[1].len() != 2 || p[2].len() != 2 {
        return Err(TemplateError::Value(value.into()));
    }
    let year = p[0]
        .parse::<u32>()
        .map_err(|_| TemplateError::Value(value.into()))?;
    let month = p[1]
        .parse::<u32>()
        .map_err(|_| TemplateError::Value(value.into()))?;
    let day = p[2]
        .parse::<u32>()
        .map_err(|_| TemplateError::Value(value.into()))?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if day == 0 || day > days {
        return Err(TemplateError::Value(value.into()));
    }
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('Y') => out.push_str(p[0]),
                Some('m') => out.push_str(p[1]),
                Some('d') => out.push_str(p[2]),
                Some('%') => out.push('%'),
                _ => return Err(TemplateError::Transform(pattern.into())),
            }
        } else {
            out.push(c)
        }
    }
    Ok(out)
}

/// Decimal formatting without binary floating-point or host-locale behavior.
fn format_decimal(value: &str, decimals: usize) -> Result<String, TemplateError> {
    let normalized = value.trim().replace(',', ".");
    let (negative, unsigned) = normalized
        .strip_prefix('-')
        .map_or((false, normalized.as_str()), |v| (true, v));
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(TemplateError::Value(value.into()));
    }
    let mut whole = whole
        .parse::<u128>()
        .map_err(|_| TemplateError::Value(value.into()))?;
    let mut kept = fraction
        .bytes()
        .take(decimals)
        .map(|b| b - b'0')
        .collect::<Vec<_>>();
    kept.resize(decimals, 0);
    if fraction
        .as_bytes()
        .get(decimals)
        .is_some_and(|digit| *digit >= b'5')
    {
        let mut carry = true;
        for digit in kept.iter_mut().rev() {
            if *digit < 9 {
                *digit += 1;
                carry = false;
                break;
            }
            *digit = 0;
        }
        if carry {
            whole = whole
                .checked_add(1)
                .ok_or_else(|| TemplateError::Value(value.into()))?;
        }
    }
    let sign = if negative && (whole != 0 || kept.iter().any(|v| *v != 0)) {
        "-"
    } else {
        ""
    };
    if decimals == 0 {
        Ok(format!("{sign}{whole}"))
    } else {
        Ok(format!(
            "{sign}{whole}.{}",
            kept.into_iter()
                .map(|v| char::from(b'0' + v))
                .collect::<String>()
        ))
    }
}
