use source_downloader_sdk::component::PatternVariables;
use std::collections::HashMap;

/// 合并时每个候选值的上下文
#[derive(Debug)]
pub struct Candidate<'a> {
    pub value: &'a String,
    pub accuracy: i32,
    pub index: usize, // 用于在权重相同时保持原始顺序
}

// --- 冲突策略 Trait ---

pub trait ConflictStrategy: Sync + Send {
    fn resolve(&self, key: &str, candidates: &[Candidate]) -> String;
}

// --- 默认策略实现 ---

/// 简单策略：后来的覆盖前面的
pub struct AnyStrategy;
impl ConflictStrategy for AnyStrategy {
    fn resolve(&self, _: &str, candidates: &[Candidate]) -> String {
        candidates
            .last()
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }
}

/// 精度策略：取 accuracy 最高的，精度相同时取第一个 (index最小)
#[allow(unused)]
pub struct AccuracyStrategy;
impl ConflictStrategy for AccuracyStrategy {
    fn resolve(&self, _: &str, candidates: &[Candidate]) -> String {
        candidates
            .iter()
            .max_by(|a, b| a.accuracy.cmp(&b.accuracy).then(b.index.cmp(&a.index)))
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }
}

/// 投票策略：出现次数最多的胜出，票数相同时取第一个
pub struct VoteStrategy;
impl ConflictStrategy for VoteStrategy {
    fn resolve(&self, _: &str, candidates: &[Candidate]) -> String {
        let mut counts: HashMap<&String, (usize, usize)> = HashMap::new(); // Value -> (Count, FirstIndex)
        for c in candidates {
            let entry = counts.entry(c.value).or_insert((0, c.index));
            entry.0 += 1;
        }
        counts
            .into_iter()
            .max_by(|(_, (cnt1, idx1)), (_, (cnt2, idx2))| cnt1.cmp(cnt2).then(idx2.cmp(idx1)))
            .map(|(v, _)| v.clone())
            .unwrap_or_default()
    }
}

/// 智能策略：精度优先 > 票数优先 > 顺序优先
pub struct SmartStrategy;
impl ConflictStrategy for SmartStrategy {
    fn resolve(&self, _: &str, candidates: &[Candidate]) -> String {
        let mut stats: HashMap<&String, (i32, usize, usize)> = HashMap::new(); // Value -> (MaxAcc, Count, MinIdx)
        for c in candidates {
            let entry = stats.entry(c.value).or_insert((c.accuracy, 0, c.index));
            entry.0 = entry.0.max(c.accuracy);
            entry.1 += 1;
        }
        stats
            .into_iter()
            .max_by(|(_, (acc1, cnt1, idx1)), (_, (acc2, cnt2, idx2))| {
                acc1.cmp(acc2).then(cnt1.cmp(cnt2)).then(idx2.cmp(idx1))
            })
            .map(|(v, _)| v.clone())
            .unwrap_or_default()
    }
}

// --- 核心聚合器 ---

pub struct VariableAggregation {
    pub strategy: Box<dyn ConflictStrategy>,
    pub name_replace: HashMap<String, String>,
}

impl VariableAggregation {
    pub fn new(strategy: Box<dyn ConflictStrategy>, name_replace: HashMap<String, String>) -> Self {
        Self {
            strategy,
            name_replace,
        }
    }

    /// 合并单层变量 (如 itemVariables)
    /// inputs: [(accuracy, variables)]
    pub fn merge(&self, inputs: &[(i32, PatternVariables)]) -> PatternVariables {
        let mut grouped: HashMap<String, Vec<Candidate>> = HashMap::new();

        for (idx, (acc, vars)) in inputs.iter().enumerate() {
            for (k, v) in vars {
                let final_key = self
                    .name_replace
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| k.clone());
                grouped.entry(final_key).or_default().push(Candidate {
                    value: v,
                    accuracy: *acc,
                    index: idx,
                });
            }
        }

        grouped
            .into_iter()
            .map(|(k, candidates)| (k.clone(), self.strategy.resolve(&k, &candidates)))
            .collect()
    }

    /// 合并多文件变量 (如 fileVariables)
    /// inputs: [(accuracy, Vec<PatternVariables>)]
    pub fn merge_files(&self, inputs: &[(i32, Vec<PatternVariables>)]) -> Vec<PatternVariables> {
        if inputs.is_empty() {
            return vec![];
        }

        let file_count = inputs.iter().map(|i| i.1.len()).max().unwrap_or(0);

        (0..file_count)
            .map(|f_idx| {
                let slice: Vec<(i32, PatternVariables)> = inputs
                    .iter()
                    .map(|(acc, files)| (*acc, files.get(f_idx).cloned().unwrap_or_default()))
                    .collect();
                self.merge(&slice)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vote() {
        let agg = VariableAggregation::new(
            Box::new(VoteStrategy),
            vec![("seasonNumber".into(), "season".into())]
                .into_iter()
                .collect(),
        );

        // 模拟输入数据: (accuracy, variables)
        let inputs = vec![
            (
                2,
                [
                    ("seasonNumber".into(), "01".into()),
                    ("test1".into(), "2".into()),
                ]
                .into(),
            ),
            (
                2,
                [
                    ("seasonNumber".into(), "01".into()),
                    ("test2".into(), "2".into()),
                ]
                .into(),
            ),
            (
                2,
                [("season".into(), "02".into()), ("test3".into(), "2".into())].into(),
            ),
        ];

        let result = agg.merge(&inputs);
        assert_eq!(result.get("season").unwrap(), "01"); // 01出现了两次，胜出
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_smart() {
        let agg = VariableAggregation::new(Box::new(SmartStrategy), HashMap::new());

        let inputs = vec![
            (0, [("season".into(), "00".into())].into()), // 精度低
            (2, [("season".into(), "01".into())].into()), // 精度高，但只有1票
            (2, [("season".into(), "02".into())].into()), // 精度高，且与下面一个同值，共2票
            (1, [("season".into(), "02".into())].into()),
        ];

        let result = agg.merge(&inputs);
        assert_eq!(result.get("season").unwrap(), "02");
    }

    #[test]
    fn test_same_accuracy_order() {
        let agg = VariableAggregation::new(Box::new(SmartStrategy), HashMap::new());

        let inputs = vec![
            (2, [("season".into(), "00".into())].into()), // 第一个
            (2, [("season".into(), "01".into())].into()), // 第二个，精度相同
        ];

        let result = agg.merge(&inputs);
        // 应该保持第一个的值
        assert_eq!(result.get("season").unwrap(), "00");
    }

    #[test]
    fn test_merge_files() {
        let agg = VariableAggregation::new(Box::new(VoteStrategy), HashMap::new());
        let provider_files = vec![
            (
                2,
                vec![
                    [("ep".into(), "01".into())].into(),
                    [("ep".into(), "02".into())].into(),
                ],
            ),
            (
                2,
                vec![
                    [("ep".into(), "01".into())].into(),
                    [("ep".into(), "02.5".into())].into(),
                ],
            ),
            (
                2,
                vec![
                    [("ep".into(), "01".into())].into(),
                    [("ep".into(), "02.5".into())].into(),
                ],
            ),
        ];

        let results = agg.merge_files(&provider_files);
        assert_eq!(results[0].get("ep").unwrap(), "01");
        assert_eq!(results[1].get("ep").unwrap(), "02.5");
    }
}
