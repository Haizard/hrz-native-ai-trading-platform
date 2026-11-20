//! Continuous-time Markov chains for irregular market events.

use crate::transition::Matrix;

fn multiply(left: &Matrix, right: &Matrix) -> Matrix {
    let n = left.len();
    let mut out = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        for k in 0..n {
            if left[i][k] == 0.0 {
                continue;
            }
            for j in 0..n {
                out[i][j] += left[i][k] * right[k][j];
            }
        }
    }
    out
}

/// `P(t) = e^{Qt}` via uniformization.
///
/// `Q` is the generator matrix: off-diagonals are transition rates, diagonals
/// are negative row sums. A zero generator is the identity (no event can
/// happen), which is the safe answer for a quiet book.
pub fn exp_generator(generator: &Matrix, time: f64) -> Result<Matrix, String> {
    let n = generator.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    if generator.iter().any(|row| row.len() != n) {
        return Err("the CTMC generator must be square".into());
    }
    if time < 0.0 || !time.is_finite() {
        return Err("CTMC time must be finite and non-negative".into());
    }
    let lambda = generator
        .iter()
        .enumerate()
        .map(|(i, row)| -row[i])
        .fold(0.0f64, f64::max)
        * 1.05;
    if lambda <= 0.0 {
        return Ok((0..n)
            .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
            .collect());
    }

    let mut transition: Matrix = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| {
                    let value = if i == j {
                        1.0 + generator[i][j] / lambda
                    } else {
                        generator[i][j] / lambda
                    };
                    if value.is_finite() {
                        value
                    } else {
                        0.0
                    }
                })
                .collect()
        })
        .collect();

    let mut result: Matrix = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    let identity: Matrix = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    for row in result.iter_mut() {
        for value in row.iter_mut() {
            *value = 0.0;
        }
    }
    let mut term = identity; // P^0
    let mut weight = (-lambda * time).exp();
    let mut total_weight = weight;
    for i in 0..n {
        for j in 0..n {
            result[i][j] += weight * term[i][j];
        }
    }
    for step in 1..256 {
        term = multiply(&term, &transition);
        weight *= lambda * time / step as f64;
        for i in 0..n {
            for j in 0..n {
                result[i][j] += weight * term[i][j];
            }
        }
        total_weight += weight;
        if !weight.is_finite() || weight.abs() < 1e-15 {
            break;
        }
    }
    if !total_weight.is_finite() || total_weight <= 0.0 {
        return Err("CTMC uniformization did not converge".into());
    }
    for row in &mut result {
        for value in row.iter_mut() {
            *value = (*value / total_weight).max(0.0);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_generator_is_the_identity() {
        let identity = exp_generator(&vec![vec![0.0, 0.0], vec![0.0, 0.0]], 5.0).unwrap();
        assert_eq!(identity, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn a_two_state_generator_reaches_its_stationary_mix() {
        // Rates 0.1 and 0.4 converge to [0.8, 0.2].
        let generator = vec![vec![-0.1, 0.1], vec![0.4, -0.4]];
        let matrix = exp_generator(&generator, 20.0).unwrap();
        assert!((matrix[0][0] - 0.8).abs() < 0.01, "{matrix:?}");
        assert!((matrix[1][0] - 0.8).abs() < 0.01, "{matrix:?}");
        for row in matrix {
            assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        }
    }
}
