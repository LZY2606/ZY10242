use crate::domain::{Point, Polygon};
use crate::geometry;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon::new(vec![
        Point::new(x0, y0), Point::new(x1, y0), Point::new(x1, y1), Point::new(x0, y1),
    ]).unwrap()
}

#[test]
fn half_open_rectangle_edges() {
    let p = rect(0.0, 0.0, 10.0, 10.0);
    assert!(geometry::contains(&p, Point::new(5.0, 5.0)));
    // 底边、左边计入；顶边、右边计出
    assert!(geometry::contains(&p, Point::new(5.0, 0.0)));
    assert!(geometry::contains(&p, Point::new(0.0, 5.0)));
    assert!(!geometry::contains(&p, Point::new(5.0, 10.0)));
    assert!(!geometry::contains(&p, Point::new(10.0, 5.0)));
    // 角点也唯一定属
    assert!(geometry::contains(&p, Point::new(0.0, 0.0)));
    assert!(!geometry::contains(&p, Point::new(10.0, 10.0)));
}

#[test]
fn shared_edge_is_partition() {
    // 两个 CCW 矩形共享竖边 x=0；左/右遍历方向相反
    let left = Polygon::new(vec![
        Point::new(-10.0, -10.0), Point::new(0.0, -10.0),
        Point::new(0.0, 10.0), Point::new(-10.0, 10.0),
    ]).unwrap();
    let right = Polygon::new(vec![
        Point::new(0.0, -10.0), Point::new(10.0, -10.0),
        Point::new(10.0, 10.0), Point::new(0.0, 10.0),
    ]).unwrap();
    geometry::validate_shared_edges(&[("L".into(), left.clone()), ("R".into(), right.clone())]).unwrap();
    // 共享边内部：恰好属于一侧（半开 → 右侧），不双计
    for y in [-9.9, -3.7, 0.0, 4.2, 9.9] {
        let q = Point::new(0.0, y);
        assert!(!geometry::contains(&left, q), "内部边点不属左 y={y}");
        assert!(geometry::contains(&right, q), "内部边点属右 y={y}");
    }
    // 共享边两个端点按统一的端点半开约定各落一侧/外部，仍不双计
    let bottom = Point::new(0.0, -10.0);
    let top = Point::new(0.0, 10.0);
    assert!(!(geometry::contains(&left, bottom) && geometry::contains(&right, bottom)));
    assert!(!(geometry::contains(&left, top) && geometry::contains(&right, top)));
    // 下端点恰好归一侧
    assert_ne!(geometry::contains(&left, bottom), geometry::contains(&right, bottom));
}

#[test]
fn winding_direction_independent() {
    let cw: Vec<Point> = vec![
        Point::new(0.0, 0.0), Point::new(0.0, 10.0),
        Point::new(10.0, 10.0), Point::new(10.0, 0.0),
    ];
    let p = Polygon::new(cw).unwrap();
    assert!(geometry::contains(&p, Point::new(5.0, 5.0)));
    assert!(!geometry::contains(&p, Point::new(5.0, 10.0)));
}

#[test]
fn near_miss_shared_edge_rejected() {
    let left = Polygon::new(vec![
        Point::new(-10.0, -10.0), Point::new(0.0, -10.0),
        Point::new(0.0, 10.0), Point::new(-10.0, 10.0),
    ]).unwrap();
    // 右侧多边形共享边上端点漂移 1e-12 —— 拒绝而不是默默歧义
    let right = Polygon::new(vec![
        Point::new(0.0, -10.0 + 1e-12), Point::new(10.0, -10.0),
        Point::new(10.0, 10.0), Point::new(0.0, 10.0),
    ]).unwrap();
    assert!(geometry::validate_shared_edges(&[("L".into(), left), ("R".into(), right)]).is_err());
}
