//! Fit the hardware key arrangement to measured points in display coordinates.
use crate::Point;
use std::collections::BTreeMap;

pub fn fit(
    current: &BTreeMap<String, Point>,
    reference: &BTreeMap<String, Point>,
) -> Result<BTreeMap<String, Point>, String> {
    if current.len() != reference.len() || current.len() < 3 {
        return Err("校准点不完整".into());
    }
    let pairs: Vec<_> = current
        .iter()
        .map(|(id, point)| {
            reference
                .get(id)
                .map(|p| (*p, *point))
                .ok_or_else(|| format!("缺少键位 {id}"))
        })
        .collect::<Result<_, _>>()?;
    let mut matrix = [[0.; 3]; 3];
    let mut bx = [0.; 3];
    let mut by = [0.; 3];
    for (r, p) in &pairs {
        if ![r.x, r.y, p.x, p.y].iter().all(|n| n.is_finite()) {
            return Err("校准坐标无效".into());
        }
        let row = [1., r.x, r.y];
        for i in 0..3 {
            bx[i] += row[i] * p.x;
            by[i] += row[i] * p.y;
            for j in 0..3 {
                matrix[i][j] += row[i] * row[j];
            }
        }
    }
    let solve = |rhs: [f64; 3]| -> Result<[f64; 3], String> {
        let mut a = matrix;
        let mut b = rhs;
        for k in 0..3 {
            let pivot = (k..3)
                .max_by(|&i, &j| a[i][k].abs().total_cmp(&a[j][k].abs()))
                .unwrap();
            a.swap(k, pivot);
            b.swap(k, pivot);
            if a[k][k].abs() < 1e-10 {
                return Err("键位排列不足以拟合".into());
            }
            for i in k + 1..3 {
                let f = a[i][k] / a[k][k];
                for j in k..3 {
                    a[i][j] -= f * a[k][j];
                }
                b[i] -= f * b[k];
            }
        }
        let mut out = [0.; 3];
        for i in (0..3).rev() {
            out[i] = (b[i] - (i + 1..3).map(|j| a[i][j] * out[j]).sum::<f64>()) / a[i][i];
        }
        Ok(out)
    };
    let x = solve(bx)?;
    let y = solve(by)?;
    let mut fitted: BTreeMap<_, _> = reference
        .iter()
        .map(|(id, p)| {
            (
                id.clone(),
                Point {
                    x: x[0] + x[1] * p.x + x[2] * p.y,
                    y: y[0] + y[1] * p.x + y[2] * p.y,
                },
            )
        })
        .collect();
    for axis in 0..2 {
        let value = |p: &Point| if axis == 0 { p.x } else { p.y };
        let bounds = |points: &BTreeMap<String, Point>| {
            points
                .values()
                .map(value)
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v), hi.max(v))
                })
        };
        let (lo, hi) = bounds(current);
        let (fl, fh) = bounds(&fitted);
        if hi - lo < 1e-9 || fh - fl < 1e-9 {
            return Err("校准范围过小".into());
        }
    }
    for (code, point) in &mut fitted {
        point.x = point.x.clamp(0., 1.);
        point.y = point.y.clamp(0., 1.);
        let original = current[code];
        // Avoid a save preview when the fit only changes floating-point noise.
        if (point.x - original.x).abs() < 1e-10 && (point.y - original.y).abs() < 1e-10 {
            *point = original;
        }
    }
    Ok(fitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regularizes_noisy_rows_without_rescaling_the_fit_to_outliers() {
        let reference: BTreeMap<_, _> = (0..24)
            .map(|i| {
                (
                    format!("K{i}"),
                    Point {
                        x: (i % 6) as f64 / 5.,
                        y: (i / 6) as f64 / 3.,
                    },
                )
            })
            .collect();
        let current: BTreeMap<_, _> = reference
            .iter()
            .enumerate()
            .map(|(i, (k, p))| {
                (
                    k.clone(),
                    Point {
                        x: 0.1 + p.x * 0.7 + if i % 3 == 0 { 0.005 } else { 0. },
                        y: 0.2 + p.y * 0.5 + if i % 4 == 0 { 0.004 } else { 0. },
                    },
                )
            })
            .collect();
        let result = fit(&current, &reference).unwrap();
        let error = |points: &BTreeMap<String, Point>| {
            reference
                .iter()
                .map(|(code, p)| {
                    let actual = points[code];
                    (actual.x - (0.1 + p.x * 0.7)).powi(2)
                        + (actual.y - (0.2 + p.y * 0.5)).powi(2)
                })
                .sum::<f64>()
        };
        assert!(error(&result) < error(&current));
        assert_eq!(fit(&result, &reference).unwrap(), result);
        assert_ne!(current, result);
    }
    #[test]
    fn rejects_degenerate_reference_and_missing_keys() {
        let line = (0..4)
            .map(|i| (i.to_string(), Point { x: i as f64, y: 0. }))
            .collect();
        assert!(fit(&line, &line).is_err());
        assert!(fit(&BTreeMap::new(), &line).is_err());
    }
}
