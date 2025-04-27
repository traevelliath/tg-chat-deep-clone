#[derive(clap::Parser)]
pub struct Config {
    /// The connection URL for the SQLite database
    #[clap(long, env)]
    pub database_url: String,

    #[clap(long, env)]
    pub api_id: i32,

    #[clap(long, env)]
    pub api_hash: String,

    #[clap(long, env, value_parser, num_args = 1.., value_delimiter = ',')]
    pub admins: Vec<i64>,

    #[clap(long, env, default_value = "4")]
    pub concurrency: usize,
}
