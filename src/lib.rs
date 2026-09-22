pub mod db;
pub mod engine;
pub mod geometry;
pub mod model;
pub mod web;

pub use db::Db;
pub use engine::EngineError;

pub fn bootstrap(db_path: &str) -> Result<Db, EngineError> {
    let db = Db::open(db_path)?;
    if db.meta_get("fixture_version").is_none() {
        db.reseed(&model::fixture())?;
    }
    Ok(db)
}
