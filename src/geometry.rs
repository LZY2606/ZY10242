//! 统一的半开多边形几何规则。
//!
//! 规则（半开区间，与标准光栅化/奇偶规则一致）：
//! 对从查询点水平向右的射线，一条边 (a -> b) 被计为“向上穿过射线”当且仅当
//!   a.y <= p.y < b.y  （仅下端点包含、上端点排除）
//! 向下穿过则相反。交点相对 p 的左右关系用方向叉积（orientation）的符号判定，
//! 不计算交点坐标，从而共享边两侧的两个多边形对该边得到严格互补的计数。
//!
//! 后果：
//! * 点恰好落在边上：边本身不计入，但共享该边的另一个多边形会按同一规则计入其一，
//!   因此相邻门不会双重计数，也不会都漏掉（前提是共享边几何一致）。
//! * 点恰好落在共享顶点上：由端点半开约定唯一定属。
//! * 规则与多边形顶点方向无关，也与求值顺序无关。

use crate::domain::{Point, Polygon, PolygonError};

/// `(b-a) × (p-a)` 的符号：正 = p 在有向边左侧。
#[inline]
fn orient(a: &Point, b: &Point, p: &Point) -> f64 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

pub fn contains(poly: &Polygon, p: Point) -> bool {
    let n = poly.vertices.len();
    let mut winding = 0i64;
    for i in 0..n {
        let a = &poly.vertices[i];
        let b = &poly.vertices[(i + 1) % n];
        if a.y <= p.y {
            if b.y > p.y && orient(a, b, &p) > 0.0 {
                // 向上穿过射线且交点在 p 右侧
                winding += 1;
            }
        } else if b.y <= p.y && orient(a, b, &p) < 0.0 {
            // 向下穿过射线且交点在 p 右侧
            winding -= 1;
        }
    }
    winding != 0
}

/// 两个多边形是否存在一条完全重合（端点到 f64 位模式一致）的边，顺序可反向。
fn shared_edge(p1: &Polygon, p2: &Polygon) -> Option<(usize, usize)> {
    let n1 = p1.vertices.len();
    let n2 = p2.vertices.len();
    for i in 0..n1 {
        let a1 = p1.vertices[i];
        let b1 = p1.vertices[(i + 1) % n1];
        for j in 0..n2 {
            let a2 = p2.vertices[j];
            let b2 = p2.vertices[(j + 1) % n2];
            if (a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2) {
                return Some((i, j));
            }
        }
    }
    None
}

fn on_segment(a: &Point, b: &Point, p: &Point) -> bool {
    orient(a, b, p) == 0.0
        && p.x >= a.x.min(b.x)
        && p.x <= a.x.max(b.x)
        && p.y >= a.y.min(b.y)
        && p.y <= a.y.max(b.y)
}

/// 在一批同层级的门之间校验半开规则成立的前提：
/// 若两个多边形的边界“看起来重叠”（有顶点落在对方边界上），
/// 则必须存在一条逐端点一致的共享边，否则拒绝 —— 绝不带着歧义继续计数。
pub fn validate_shared_edges(polys: &[(String, Polygon)]) -> Result<(), PolygonError> {
    for i in 0..polys.len() {
        for j in (i + 1)..polys.len() {
            let (_, p1) = &polys[i];
            let (_, p2) = &polys[j];
            let mut boundaries_touch = false;
            for v in &p1.vertices {
                for k in 0..p2.vertices.len() {
                    let a = &p2.vertices[k];
                    let b = &p2.vertices[(k + 1) % p2.vertices.len()];
                    if on_segment(a, b, v) {
                        boundaries_touch = true;
                    }
                }
            }
            if boundaries_touch && shared_edge(p1, p2).is_none() {
                return Err(PolygonError::SharedEdgeMismatch);
            }
        }
    }
    Ok(())
}
