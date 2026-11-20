use super::*;

pub(super) async fn models(
    runtime: &Runtime,
    key: &str,
    account_id: i64,
    test_models: bool,
    refresh: bool,
) -> Result<Value> {
    Ok(if test_models {
        let row = runtime.actions.queries.private_account(account_id).await?;
        if crate::model_target::supported(&row) {
            json!(
                runtime
                    .actions
                    .model_test_candidates(&row, key, refresh)
                    .await?
            )
        } else {
            let identity = runtime.actions.candidate_source(&row, key).await?;
            let actions = runtime.actions.clone();
            let key = key.to_owned();
            let fetch = async move {
                let result: Result<Vec<Value>> = async {
                    let models = match crate::model_directory::account_models_read(
                        &actions.queries.db,
                        account_id,
                    )
                    .await
                    {
                        Ok(models) => models,
                        Err(error) if error.status.as_u16() == 422 => {
                            let data = actions.admin_models(account_id, &key).await?;
                            let items = data
                                .as_array()
                                .ok_or_else(|| Error::new(502, "模型列表格式无效"))?;
                            if items.len() > 10_000 {
                                return Err(Error::new(502, "上游模型列表超过限制"));
                            }
                            crate::actions::filter_models(items, &[])
                        }
                        Err(error) => return Err(error),
                    };
                    Ok(models)
                }
                .await;
                result.map_err(|error| {
                    if error.status.is_server_error()
                        && error.detail["code"] != crate::model_directory::DIRECTORY_REVOKED
                    {
                        crate::model_candidates::Failure::temporary(error)
                    } else {
                        crate::model_candidates::Failure::permanent(error)
                    }
                })
            };
            json!(
                runtime
                    .actions
                    .candidates
                    .get(identity, refresh, fetch)
                    .await?
            )
        }
    } else {
        json!(runtime.actions.models(account_id, key, false).await?)
    })
}
