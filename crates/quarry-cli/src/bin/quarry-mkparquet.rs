//! Test-fixture helper: convert a CSV file to Parquet with DataFusion.
//! Used by tools/test_sidecar.sh; not part of the user-facing CLI.
//! Usage: quarry-mkparquet <input.csv> <output.parquet>

use datafusion::prelude::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("input.csv");
    let output = args.next().expect("output.parquet");
    let ctx = SessionContext::new();
    ctx.register_csv("t", &input, CsvReadOptions::new().has_header(true))
        .await?;
    let df = ctx.sql("SELECT * FROM t").await?;
    df.write_parquet(
        &output,
        datafusion::dataframe::DataFrameWriteOptions::new(),
        None,
    )
    .await?;
    eprintln!("wrote {output}");
    Ok(())
}
