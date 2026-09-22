//! 荧光补偿。
//!
//! 补偿矩阵以通道名显式标注：行 = 输出通道，列 = 输入（源）通道。
//! 应用前要求矩阵的行集合与列集合都与数据通道集合**完全一致**；
//! 缺通道、多通道、顺序不同（即使集合相同）都拒绝按位置继续乘法，
//! 必须由调用方提供与数据通道同序的显式映射（本实现即按名字逐项查找）。

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompensationError {
    /// 矩阵标注的通道与数据通道集合不一致
    ChannelMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    NotSquare,
}

#[derive(Debug, Clone)]
pub struct Compensation {
    /// 矩阵显式声明的通道顺序
    pub channels: Vec<String>,
    /// rows[i][j]：输出 channels[i] 对输入 channels[j] 的系数
    pub rows: Vec<Vec<f64>>,
}

impl Compensation {
    pub fn validate(&self, data_channels: &[String]) -> Result<(), CompensationError> {
        if self.rows.len() != self.channels.len()
            || self.rows.iter().any(|r| r.len() != self.channels.len())
        {
            return Err(CompensationError::NotSquare);
        }
        let declared: BTreeSet<&String> = self.channels.iter().collect();
        let actual: BTreeSet<&String> = data_channels.iter().collect();
        let missing: Vec<String> = actual
            .difference(&declared)
            .map(|s| (*s).clone())
            .collect();
        let unexpected: Vec<String> = declared
            .difference(&actual)
            .map(|s| (*s).clone())
            .collect();
        if !missing.is_empty() || !unexpected.is_empty() {
            return Err(CompensationError::ChannelMismatch { missing, unexpected });
        }
        Ok(())
    }

    /// 按通道名查表执行 y_i = sum_j M[i,j] * x_j。
    ///
    /// 传入通道顺序无关：每一项都按名字定位。矩阵未通过 `validate` 时返回错误。
    pub fn apply(
        &self,
        data_channels: &[String],
        values: &[f64],
    ) -> Result<Vec<f64>, CompensationError> {
        self.validate(data_channels)?;
        let get = |name: &str| -> Option<f64> {
            data_channels
                .iter()
                .position(|c| c == name)
                .map(|idx| values[idx])
        };
        let mut out = Vec::with_capacity(data_channels.len());
        for (i, out_name) in self.channels.iter().enumerate() {
            debug_assert!(data_channels.contains(out_name));
            let mut acc = 0.0;
            for (j, src_name) in self.channels.iter().enumerate() {
                // 缺通道在这里不可能发生（validate 已保证），但不依赖位置，
                // 即使调用方调换了数据列顺序，仍按名字取到正确的值。
                acc += self.rows[i][j] * get(src_name).expect("validated channel");
            }
            out.push(acc);
        }
        // 输出按 data_channels 的顺序返回，与 self.channels 的声明顺序解耦
        let mut ordered = Vec::with_capacity(data_channels.len());
        for name in data_channels {
            let idx = self.channels.iter().position(|c| c == name).unwrap();
            ordered.push(out[idx]);
        }
        Ok(ordered)
    }

    /// 单位矩阵（fixture 默认补偿）。
    pub fn identity(channels: &[String]) -> Self {
        let mut rows = vec![vec![0.0; channels.len()]; channels.len()];
        for i in 0..channels.len() {
            rows[i][i] = 1.0;
        }
        Compensation {
            channels: channels.to_vec(),
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chans() -> Vec<String> {
        vec!["A".into(), "B".into(), "C".into()]
    }

    #[test]
    fn reordered_columns_still_applied_by_name() {
        // 矩阵按 C,A,B 顺序声明，数据按 A,B,C 提供 —— 集合相同但顺序不同，
        // 结果必须与按名字查表一致，而不是按位置相乘。
        let comp = Compensation {
            channels: vec!["C".into(), "A".into(), "B".into()],
            rows: vec![
                vec![1.0, 0.0, 0.0],
                vec![0.0, 1.0, 0.0],
                vec![0.0, 0.0, 1.0],
            ],
        };
        let data = vec!["A".into(), "B".into(), "C".into()];
        let out = comp.apply(&data, &[10.0, 20.0, 30.0]).unwrap();
        assert_eq!(out, vec![10.0, 20.0, 30.0]);
    }

    #[test]
    fn missing_channel_is_rejected_not_positional() {
        let comp = Compensation {
            channels: vec!["A".into(), "B".into()],
            rows: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
        };
        let err = comp.apply(&chans(), &[1.0, 2.0, 3.0]).unwrap_err();
        match err {
            CompensationError::ChannelMismatch { missing, .. } => assert_eq!(missing, vec!["C"]),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn spillover_matrix_mixes_by_name() {
        // B 通道对 A 有 10% 溢出补偿项
        let mut comp = Compensation::identity(&chans());
        comp.rows[1][0] = -0.1; // 输出 B += -0.1 * 输入 A
        let out = comp.apply(&chans(), &[100.0, 50.0, 5.0]).unwrap();
        assert!((out[0] - 100.0).abs() < 1e-12);
        assert!((out[1] - 40.0).abs() < 1e-12);
        assert!((out[2] - 5.0).abs() < 1e-12);
    }
}
