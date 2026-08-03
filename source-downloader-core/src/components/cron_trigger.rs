use crate::components::holding_task_trigger::HoldingTaskTrigger;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, Trigger,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::time::{Duration as TimeDuration, OffsetDateTime, Weekday};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex};
use tokio::task::AbortHandle;

pub struct CronTriggerSupplier;
pub const SUPPLIER: CronTriggerSupplier = CronTriggerSupplier;

impl ComponentSupplier for CronTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("cron".to_owned())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError>
    {
        let expression = props
            .get("expression")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::from("Missing 'expression' property"))?;
        let expression =
            CronExpression::parse(expression).map_err(ComponentError::new)?;
        Ok(Arc::new(CronTrigger::new(expression)))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(source_downloader_sdk::SdComponent)]
#[component(Trigger, Stateful)]
pub struct CronTrigger {
    expression: CronExpression,
    holding: HoldingTaskTrigger,
    worker_handle: Mutex<Option<AbortHandle>>,
}

impl CronTrigger {
    fn new(expression: CronExpression) -> Self {
        Self {
            expression,
            holding: HoldingTaskTrigger::new(),
            worker_handle: Mutex::new(None),
        }
    }
}

impl Debug for CronTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronTrigger")
            .field("expression", &self.expression)
            .field("task_count", &self.holding.tasks().len())
            .field(
                "running",
                &self.worker_handle.lock().is_ok_and(|guard| guard.is_some()),
            )
            .finish()
    }
}

impl Display for CronTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("cron")
    }
}

impl Stateful for CronTrigger {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        Some(self.holding.state_detail())
    }
}

impl Trigger for CronTrigger {
    fn start(&self) {
        let Ok(mut worker_handle) = self.worker_handle.lock() else {
            return;
        };
        let task_groups = group_tasks(self.holding.tasks());
        let expression = self.expression.clone();

        *worker_handle = Some(
            tokio::spawn(async move {
                let mut now = OffsetDateTime::now_utc();
                loop {
                    let Some(next) = expression.next_after(now) else {
                        tracing::error!("Cron expression has no future execution time");
                        return;
                    };
                    let delay = (next - OffsetDateTime::now_utc())
                        .whole_nanoseconds()
                        .max(1) as u64;
                    tokio::time::sleep(std::time::Duration::from_nanos(delay)).await;
                    for group in &task_groups {
                        for task in group {
                            if let Err(error) = task.run().await {
                                tracing::error!(
                                    task = %task.name(),
                                    error = %error,
                                    "Task processing failed"
                                );
                            }
                        }
                    }
                    now = OffsetDateTime::now_utc();
                }
            })
            .abort_handle(),
        );
        tracing::info!(expression = %self.expression, "Cron trigger started");
    }

    fn stop(&self) {
        let Ok(mut worker_handle) = self.worker_handle.lock() else {
            return;
        };
        if let Some(handle) = worker_handle.take() {
            handle.abort();
            tracing::info!("Cron trigger stopped");
        }
    }

    fn add_task(&self, task: Arc<dyn ProcessTask>) {
        self.holding.add_task(task);
    }

    fn remove_task(&self, task: Arc<dyn ProcessTask>) {
        self.holding.remove_task(&task);
    }
}

impl Drop for CronTrigger {
    fn drop(&mut self) {
        self.stop();
    }
}

fn group_tasks(tasks: Vec<Arc<dyn ProcessTask>>) -> Vec<Vec<Arc<dyn ProcessTask>>> {
    let mut groups: Vec<(Option<String>, Vec<Arc<dyn ProcessTask>>)> = Vec::new();
    for task in tasks {
        let group = task.group();
        if let Some((_, grouped)) = groups.iter_mut().find(|(known, _)| known == &group) {
            grouped.push(task);
        } else {
            groups.push((group, vec![task]));
        }
    }
    groups.into_iter().map(|(_, tasks)| tasks).collect()
}

#[derive(Debug, Clone)]
struct CronExpression {
    seconds: CronField,
    minutes: CronField,
    hours: CronField,
    days_of_month: CronField,
    months: CronField,
    days_of_week: CronField,
}

impl Display for CronExpression {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} {} {} {}",
            self.seconds,
            self.minutes,
            self.hours,
            self.days_of_month,
            self.months,
            self.days_of_week
        )
    }
}

impl CronExpression {
    fn parse(expression: &str) -> Result<Self, String> {
        let fields: Vec<_> = expression.split_whitespace().collect();
        if fields.len() != 6 {
            return Err(String::from("Cron expression must contain six fields"));
        }
        Ok(Self {
            seconds: CronField::parse(fields[0], 0, 59, None)?,
            minutes: CronField::parse(fields[1], 0, 59, None)?,
            hours: CronField::parse(fields[2], 0, 23, None)?,
            days_of_month: CronField::parse(fields[3], 1, 31, None)?,
            months: CronField::parse(fields[4], 1, 12, Some(month_name))?,
            days_of_week: CronField::parse(fields[5], 0, 7, Some(weekday_name))?,
        })
    }

    fn matches(&self, value: OffsetDateTime) -> bool {
        let day_of_month = self.days_of_month.contains(value.day() as u32);
        let weekday = match value.weekday() {
            Weekday::Sunday => 0,
            Weekday::Monday => 1,
            Weekday::Tuesday => 2,
            Weekday::Wednesday => 3,
            Weekday::Thursday => 4,
            Weekday::Friday => 5,
            Weekday::Saturday => 6,
        };
        let day_of_week = self.days_of_week.contains(weekday)
            || (weekday == 0 && self.days_of_week.contains(7));
        let day_matches = match (self.days_of_month.wildcard, self.days_of_week.wildcard)
        {
            (true, true) => true,
            (true, false) => day_of_week,
            (false, true) => day_of_month,
            (false, false) => day_of_month || day_of_week,
        };
        self.seconds.contains(value.second() as u32)
            && self.minutes.contains(value.minute() as u32)
            && self.hours.contains(value.hour() as u32)
            && self.months.contains(value.month() as u32)
            && day_matches
    }

    fn next_after(&self, value: OffsetDateTime) -> Option<OffsetDateTime> {
        let mut candidate =
            value.replace_nanosecond(0).ok()?.checked_add(TimeDuration::SECOND)?;
        for _ in 0..(366 * 24 * 60 * 60 * 8) {
            if self.matches(candidate) {
                return Some(candidate);
            }
            candidate = candidate.checked_add(TimeDuration::SECOND)?;
        }
        None
    }
}

#[derive(Debug, Clone)]
struct CronField {
    values: Vec<u32>,
    wildcard: bool,
}

impl Display for CronField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let values = self.values.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        f.write_str(&values)
    }
}

impl CronField {
    fn parse(
        field: &str,
        minimum: u32,
        maximum: u32,
        names: Option<fn(&str) -> Option<u32>>,
    ) -> Result<Self, String> {
        let wildcard = field == "*"
            || field == "?"
            || field.starts_with("*/")
            || field.starts_with("?/");
        let mut values = Vec::new();
        for part in field.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((range, step)) => {
                    let step = step
                        .parse::<u32>()
                        .map_err(|_| format!("Invalid cron step: {step}"))?;
                    if step == 0 {
                        return Err(String::from("Cron step cannot be zero"));
                    }
                    (range, step)
                }
                None => (part, 1),
            };
            let (start, end) = if range == "*" || range == "?" {
                (minimum, maximum)
            } else if let Some((start, end)) = range.split_once('-') {
                (parse_cron_value(start, names)?, parse_cron_value(end, names)?)
            } else {
                let value = parse_cron_value(range, names)?;
                (value, if part.contains('/') { maximum } else { value })
            };
            if start < minimum || end > maximum || start > end {
                return Err(format!("Cron value out of range: {part}"));
            }
            values.extend((start..=end).step_by(step as usize));
        }
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(format!("Cron field is empty: {field}"));
        }
        Ok(Self { values, wildcard })
    }

    fn contains(&self, value: u32) -> bool {
        self.values.binary_search(&value).is_ok()
    }
}

fn parse_cron_value(
    value: &str,
    names: Option<fn(&str) -> Option<u32>>,
) -> Result<u32, String> {
    names
        .and_then(|parse| parse(value))
        .or_else(|| value.parse::<u32>().ok())
        .ok_or_else(|| format!("Invalid cron value: {value}"))
}

fn month_name(value: &str) -> Option<u32> {
    ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"]
        .iter()
        .position(|name| name.eq_ignore_ascii_case(value))
        .map(|index| index as u32 + 1)
}

fn weekday_name(value: &str) -> Option<u32> {
    ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]
        .iter()
        .position(|name| name.eq_ignore_ascii_case(value))
        .map(|index| index as u32)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_expression_requires_six_fields() {
        let error = CronExpression::parse("* * * * *").unwrap_err();

        assert_eq!(error, "Cron expression must contain six fields");
    }

    #[test]
    fn cron_expression_supports_named_months_and_weekdays() {
        let expression = CronExpression::parse("0 0 12 * JAN MON").unwrap();
        let start = OffsetDateTime::from_unix_timestamp(0).unwrap();

        assert_eq!(
            expression.next_after(start).unwrap().unix_timestamp(),
            4 * 24 * 60 * 60 + 12 * 60 * 60
        );
    }

    #[test]
    fn cron_field_rejects_zero_step() {
        let error = CronField::parse("*/0", 0, 59, None).unwrap_err();

        assert_eq!(error, "Cron step cannot be zero");
    }
}
