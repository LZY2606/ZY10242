//! 统一半开点-多边形几何。
//!
//! 规则（顶点必须按逆时针顺序给出，简单多边形）：
//! * 内部点：经典水平射线法（crossing number），边按半开方式参与，
//!   即顶点不会在相邻两条边上被重复计数。
//! * 边界点：为避免两个相邻（逆时针）多边形在共享边上双重计数，
//!   采用光栅化风格的“下行/左行边归己，上行/右行边归邻”归属：
//!   - 非水平共享边只有一侧多边形沿该边“向下行走”，恰有一个多边形归属；
//!   - 水平共享边只有一侧多边形沿该边“向左行走”，恰有一个多边形归属。
//!   - 边段内部使用 (start, end] 的半开端点：起点归上一条边，终点归本边，
//!     保证共享顶点也不会被两个多边形同时认领。
//!
//! 该规则是确定的、只依赖多边形自身顶点顺序，不需要在评估时知道“兄弟门”。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vertex {
    pub x: f64,
    pub y: f64,
}

/// 有向面积（Shoelace）。逆时针多边形为正。
pub fn signed_area(poly: &[Vertex]) -> f64 {
    let n = poly.len();
    let mut area = 0.0;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        area += a.x * b.y - b.x * a.y;
    }
    area / 2.0
}

/// 返回逆时针顺序的顶点副本；退化（共点/零面积）返回 None。
pub fn normalize_ccw(poly: &[Vertex]) -> Option<Vec<Vertex>> {
    let mut verts: Vec<Vertex> = poly.to_vec();
    if verts.len() < 3 {
        return None;
    }
    // 去除相邻重复点（含首尾）。
    loop {
        let mut deduped: Vec<Vertex> = Vec::with_capacity(verts.len());
        for v in &verts {
            if !deduped
                .last()
                .is_some_and(|last: &Vertex| last.x == v.x && last.y == v.y)
            {
                deduped.push(*v);
            }
        }
        if deduped
            .first()
            .is_some_and(|f| deduped.last().is_some_and(|l| f.x == l.x && f.y == l.y))
        {
            deduped.pop();
        }
        if deduped.len() == verts.len() {
            verts = deduped;
            break;
        }
        verts = deduped;
    }
    if verts.len() < 3 {
        return None;
    }
    let area = signed_area(&verts);
    if area.abs() < 1e-12 {
        return None;
    }
    if area < 0.0 {
        verts.reverse();
    }
    Some(verts)
}

fn cross(a: Vertex, b: Vertex, p: Vertex) -> f64 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

/// 点是否属于（归一化为逆时针的）多边形。
///
/// 统一半开规则（光栅化 Top-Left 约定，对每条边一致施加）：
/// * 非边界点：经典半开水平射线奇偶判定。
/// * 边界点：当且仅当对每条边都位于其内侧半平面，且所有共线（点落在其上）
///   的边都为“下行边（dy<0）”或“左行水平边（dy==0 且 dx<0）”时计入。
///
/// 两个逆时针相邻多边形沿共享边方向相反，因此共享边内部恰好有一方认领，
/// 绝不双计；共享顶点由半开端点约定决定归属（可能恰好归一方，也可能两方都
/// 不计——但绝不会同时计入两个门）。该规则对凸多边形精确；凹多边形的反射
/// 顶点边界处采取保守排除，仍不破坏“不双计”的唯一硬性保证。
pub fn contains(poly: &[Vertex], p: Vertex) -> bool {
    let n = poly.len();
    let mut collinear_owned: Vec<bool> = Vec::new();
    let mut on_boundary = false;
    let mut inside_all_halfplanes = true;

    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let edge_len = dx.hypot(dy).max(1e-30);
        let c = cross(a, b, p);
        if c < -1e-9 * edge_len {
            inside_all_halfplanes = false;
        }
        if c.abs() <= 1e-9 * edge_len
            && p.x >= a.x.min(b.x) - 1e-9
            && p.x <= a.x.max(b.x) + 1e-9
            && p.y >= a.y.min(b.y) - 1e-9
            && p.y <= a.y.max(b.y) + 1e-9
        {
            on_boundary = true;
            let owned = dy < -1e-12 || (dy.abs() <= 1e-12 && dx < -1e-12);
            collinear_owned.push(owned);
        }
    }

    if !on_boundary {
        return crossing_inside(poly, p);
    }
    inside_all_halfplanes && collinear_owned.iter().all(|v| *v)
}

fn crossing_inside(poly: &[Vertex], p: Vertex) -> bool {
    let n = poly.len();
    let mut inside = false;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if p.y > a.y.min(b.y) && p.y <= a.y.max(b.y) {
            let x_intersect = a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y);
            if p.x < x_intersect {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Vertex> {
        vec![
            Vertex { x: x0, y: y0 },
            Vertex { x: x1, y: y0 },
            Vertex { x: x1, y: y1 },
            Vertex { x: x0, y: y1 },
        ]
    }

    /// 不双计：任何边界点绝不可能同时属于两个相邻门（可能两方都不计，但永不双计）。
    fn assert_not_double(a: &[Vertex], b: &[Vertex], p: Vertex) {
        let hit = contains(a, p) as u32 + contains(b, p) as u32;
        assert!(hit <= 1, "boundary point {:?} double-counted ({hit})", p);
    }

    #[test]
    fn rect_basic_inside_outside() {
        let r = rect(0.0, 0.0, 10.0, 10.0);
        assert!(contains(&r, Vertex { x: 5.0, y: 5.0 }));
        assert!(!contains(&r, Vertex { x: -1.0, y: 5.0 }));
        assert!(!contains(&r, Vertex { x: 11.0, y: 5.0 }));
    }

    #[test]
    fn shared_vertical_edge_interior_exactly_once_and_vertices_never_double() {
        let left = rect(0.0, 0.0, 10.0, 10.0);
        let right = rect(10.0, 0.0, 20.0, 10.0);
        // 共享竖边内部：右矩形下行边认领，恰好一次。
        for &q in &[0.5, 2.0, 5.0, 7.999] {
            let p = Vertex { x: 10.0, y: q };
            assert!(!contains(&left, p));
            assert!(contains(&right, p));
        }
        for &q in &[0.0, 10.0] {
            assert_not_double(&left, &right, Vertex { x: 10.0, y: q });
        }
    }

    #[test]
    fn shared_horizontal_edge_interior_exactly_once_and_vertices_never_double() {
        let lower = rect(0.0, 0.0, 10.0, 10.0);
        let upper = rect(0.0, 10.0, 10.0, 20.0);
        // 共享水平边内部：下矩形左行边认领，恰好一次。
        for &q in &[0.1, 3.0, 6.0, 9.9] {
            let p = Vertex { x: q, y: 10.0 };
            assert!(contains(&lower, p));
            assert!(!contains(&upper, p));
        }
        assert_not_double(&lower, &upper, Vertex { x: 0.0, y: 10.0 });
        assert_not_double(&lower, &upper, Vertex { x: 10.0, y: 10.0 });
    }

    #[test]
    fn diagonal_split_interior_exactly_once_vertices_never_double() {
        let a = vec![
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 5.0, y: 0.0 },
            Vertex { x: 0.0, y: 5.0 },
        ];
        let b = vec![
            Vertex { x: 5.0, y: 0.0 },
            Vertex { x: 5.0, y: 5.0 },
            Vertex { x: 0.0, y: 5.0 },
        ];
        assert!(signed_area(&a) > 0.0 && signed_area(&b) > 0.0);
        for t in [0.1f64, 0.5, 0.9] {
            let p = Vertex {
                x: 5.0 * (1.0 - t),
                y: 5.0 * t,
            };
            assert!(!contains(&a, p));
            assert!(contains(&b, p));
        }
        assert_not_double(&a, &b, Vertex { x: 5.0, y: 0.0 });
        assert_not_double(&a, &b, Vertex { x: 0.0, y: 5.0 });
    }

    #[test]
    fn touching_corner_never_double() {
        // 两个仅在单点相切的矩形，切点绝不双计。
        let bl = rect(0.0, 0.0, 5.0, 5.0);
        let tr = rect(5.0, 5.0, 10.0, 10.0);
        assert_not_double(&bl, &tr, Vertex { x: 5.0, y: 5.0 });
        assert!(contains(&bl, Vertex { x: 2.0, y: 2.0 }));
        assert!(contains(&tr, Vertex { x: 7.0, y: 7.0 }));
    }

    #[test]
    fn clockwise_input_normalized() {
        let mut cw = rect(0.0, 0.0, 10.0, 10.0);
        cw.reverse();
        let ccw = normalize_ccw(&cw).unwrap();
        assert!(signed_area(&ccw) > 0.0);
        assert!(contains(&ccw, Vertex { x: 5.0, y: 5.0 }));
    }

    #[test]
    fn degenerate_polygons_rejected() {
        assert!(normalize_ccw(&[Vertex { x: 0.0, y: 0.0 }, Vertex { x: 1.0, y: 1.0 }]).is_none());
        let collinear = vec![
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 1.0, y: 1.0 },
            Vertex { x: 2.0, y: 2.0 },
        ];
        assert!(normalize_ccw(&collinear).is_none());
        let doubled = vec![
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 5.0, y: 0.0 },
            Vertex { x: 5.0, y: 5.0 },
        ];
        let norm = normalize_ccw(&doubled).unwrap();
        assert_eq!(norm.len(), 3);
    }
}
