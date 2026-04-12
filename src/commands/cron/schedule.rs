use super::*;

/// Parsed cron schedule: minute, hour, day-of-month, month, day-of-week.
/// Each field is a set of allowed values.
#[derive(Debug, Clone)]
pub(super) struct CronSchedule {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
}

impl CronSchedule {
    pub(super) fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            bail!(
                "Cron expression must have exactly 5 fields (minute hour dom month dow), got {}",
                fields.len()
            );
        }
        Ok(Self {
            minutes: parse_field(fields[0], 0, 59).context("Invalid minute field")?,
            hours: parse_field(fields[1], 0, 23).context("Invalid hour field")?,
            days_of_month: parse_field(fields[2], 1, 31).context("Invalid day-of-month field")?,
            months: parse_field(fields[3], 1, 12).context("Invalid month field")?,
            days_of_week: parse_field(fields[4], 0, 6).context("Invalid day-of-week field")?,
        })
    }

    pub(super) fn matches(&self, min: u32, hour: u32, dom: u32, month: u32, dow: u32) -> bool {
        self.minutes.contains(&min)
            && self.hours.contains(&hour)
            && self.days_of_month.contains(&dom)
            && self.months.contains(&month)
            && self.days_of_week.contains(&dow)
    }
}

/// Parse a single cron field (e.g. "*/5", "1,15", "1-5", "*").
pub(super) fn parse_field(field: &str, min: u32, max: u32) -> Result<Vec<u32>> {
    let mut values = Vec::new();

    for part in field.split(',') {
        let part = part.trim();
        if part == "*" {
            return Ok((min..=max).collect());
        }

        if let Some(step_str) = part.strip_prefix("*/") {
            let step: u32 = step_str.parse().context("Invalid step value")?;
            if step == 0 {
                bail!("Step cannot be 0");
            }
            let mut v = min;
            while v <= max {
                values.push(v);
                v += step;
            }
            continue;
        }

        if part.contains('-') {
            let (range_part, step) = if part.contains('/') {
                let sp: Vec<&str> = part.splitn(2, '/').collect();
                (
                    sp[0],
                    sp[1].parse::<u32>().context("Invalid step in range")?,
                )
            } else {
                (part, 1u32)
            };
            let bounds: Vec<&str> = range_part.splitn(2, '-').collect();
            let lo: u32 = bounds[0].parse().context("Invalid range start")?;
            let hi: u32 = bounds[1].parse().context("Invalid range end")?;
            if lo > hi || lo < min || hi > max {
                bail!("Range {lo}-{hi} out of bounds ({min}-{max})");
            }
            let mut v = lo;
            while v <= hi {
                values.push(v);
                v += step;
            }
            continue;
        }

        let v: u32 = part.parse().context("Invalid number")?;
        if v < min || v > max {
            bail!("Value {v} out of bounds ({min}-{max})");
        }
        values.push(v);
    }

    if values.is_empty() {
        bail!("Empty field");
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

/// Parse an interval string (e.g. "5m", "1h", "120s") into seconds.
pub(crate) fn interval_to_secs(interval: &str) -> Result<u64> {
    let interval = interval.trim().to_lowercase();
    let (num_str, unit) = if let Some(n) = interval.strip_suffix('m') {
        (n, 'm')
    } else if let Some(n) = interval.strip_suffix('h') {
        (n, 'h')
    } else if let Some(n) = interval.strip_suffix('s') {
        (n, 's')
    } else {
        (interval.as_str(), 'm')
    };
    let num: u64 = num_str.parse().context("Invalid interval number")?;
    if num == 0 {
        bail!("Interval must be > 0");
    }
    match unit {
        's' => Ok(num.max(60)),
        'm' => Ok(num * 60),
        'h' => Ok(num * 3600),
        _ => bail!("Unknown interval unit. Use s, m, or h"),
    }
}

#[cfg(test)]
pub(super) fn interval_to_cron(interval: &str) -> Result<String> {
    let interval = interval.trim().to_lowercase();
    let (num_str, unit) = if let Some(n) = interval.strip_suffix('m') {
        (n, 'm')
    } else if let Some(n) = interval.strip_suffix('h') {
        (n, 'h')
    } else if let Some(n) = interval.strip_suffix('s') {
        (n, 's')
    } else {
        (interval.as_str(), 'm')
    };

    let num: u32 = num_str.parse().context("Invalid interval number")?;
    if num == 0 {
        bail!("Interval must be > 0");
    }

    match unit {
        's' => {
            let minutes = num.div_ceil(60).max(1);
            if minutes >= 60 {
                Ok(format!("0 */{} * * *", minutes / 60))
            } else {
                Ok(format!("*/{minutes} * * * *"))
            }
        }
        'm' => {
            if num >= 60 {
                Ok(format!("0 */{} * * *", num / 60))
            } else {
                Ok(format!("*/{num} * * * *"))
            }
        }
        'h' => Ok(format!("0 */{num} * * *")),
        _ => bail!("Unknown interval unit. Use s, m, or h"),
    }
}
