use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::str::FromStr;

pub async fn create_pool(
  database_url: &str,
) -> Result<SqlitePool, sqlx::Error> {
  let options =
    SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);

  let pool = SqlitePoolOptions::new()
    .max_connections(5)
    .connect_with(options)
    .await?;

  Ok(pool)
}

pub async fn run_migrations(_: &SqlitePool) -> Result<(), sqlx::Error> {
  // Add any needed migrations here.

  Ok(())
}
