//! Undoes the download-only form tier ("공식 양식 받기"): with `--apply`,
//! rejects every template whose latest review is the rule's own automatic
//! publish (`download_form_v1`) and records a rejected review for each,
//! under `APPLICATION_FORMS_ACTOR_USER_ID`. Templates a person verified, or
//! reviewed after the rule, are not touched. Set `APPLICATION_FORMS_PUBLISH=off`
//! first so the next run does not publish again. Prints resource ids only.
use mazimi_worker::{application_resources::undo_download_forms, runtime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if !std::env::args().skip(1).any(|arg| arg == "--apply") {
        anyhow::bail!("usage: undo-download-forms --apply (rejects the rule's own publishes)");
    }
    let actor: uuid::Uuid = std::env::var("APPLICATION_FORMS_ACTOR_USER_ID")?
        .trim()
        .parse()?;
    anyhow::ensure!(
        !actor.is_nil(),
        "APPLICATION_FORMS_ACTOR_USER_ID must not be nil"
    );
    let pool = runtime::connect_job_pool().await?;
    let undone = undo_download_forms(&pool, actor).await?;
    for id in &undone {
        println!("{id} rejected");
    }
    println!("{:>5}  rejected", undone.len());
    pool.close().await;
    Ok(())
}
