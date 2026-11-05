//! `chu-tools rate`: the ratings refitted from the `games` table, side by side
//! with the server's `ratings` table. The fit is the server's own
//! (`chu_server::rating`), so any difference means the stored ratings are
//! stale or were computed from different games.

use std::str::FromStr;

use chu_server::rating::{self, PUBLISH_AFTER};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Ratings agree if they differ by less than this.
const TOLERANCE: f64 = 1e-6;

pub fn run(db: &str, only: Option<&str>) -> Res<bool> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let opts = SqliteConnectOptions::from_str(db)?.read_only(true);
        let pool = SqlitePool::connect_with(opts).await?;
        let leagues = match only {
            Some(l) => vec![l.to_owned()],
            None => rating::rated_leagues(&pool).await?,
        };
        let mut ok = true;
        for league in leagues {
            ok &= league_report(&pool, &league).await?;
        }
        Ok(ok)
    })
}

async fn league_report(pool: &SqlitePool, league: &str) -> Res<bool> {
    let games = rating::league_games(pool, league).await?;
    let mut fitted = rating::fit(&games);
    let stored: Vec<(String, f64, i64)> =
        sqlx::query_as("SELECT player, rating, games FROM ratings WHERE league = ?").bind(league).fetch_all(pool).await?;
    fitted.sort_by(|a, b| b.rating.total_cmp(&a.rating));
    println!("{league}: {} rated games (published from {PUBLISH_AFTER} games, * = provisional)", games.len());
    println!("{:>4} {:<32} {:>9} {:>6} {:>9}", "rank", "player", "rating", "games", "server");
    let mut ok = stored.len() == fitted.len();
    for (rank, r) in fitted.iter().enumerate() {
        let server = stored.iter().find(|s| s.0 == r.player);
        let same = server.is_some_and(|s| (s.1 - r.rating).abs() < TOLERANCE && s.2 == r.games as i64);
        ok &= same;
        let mark = if r.published() { ' ' } else { '*' };
        let server = server.map_or("missing".to_owned(), |s| format!("{:.2}", s.1));
        let flag = if same { "" } else { "  MISMATCH" };
        println!("{:>4} {:<32} {:>8.2}{mark} {:>6} {:>9}{flag}", rank + 1, r.player, r.rating, r.games, server);
    }
    for s in stored.iter().filter(|s| !fitted.iter().any(|r| r.player == s.0)) {
        println!("     {:<32} {:>9} {:>6} {:>9.2}  MISMATCH (not in the games table)", s.0, "-", "-", s.1);
    }
    println!("{league}: {}", if ok { "matches the server" } else { "DIFFERS from the server" });
    Ok(ok)
}
