//! Runs the migrations on DATABASE_URL. CI points it at template1, which Postgres copies into every database sqlx::test
//! creates, so each test starts migrated and only checks the checksums instead of running every migration again.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    sqlx::migrate!().run(&db).await?;
    // Postgres refuses to copy a template someone is still connected to.
    db.close().await;
    Ok(())
}
