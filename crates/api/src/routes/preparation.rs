use crate::{auth::AuthUser, services::application_preparation};
use axum::{
    extract::{Path, State},
    Json,
};
use sqlx::PgPool;
use uuid::Uuid;

pub(super) use application_preparation::{build, Plan};
use application_preparation::{Error, SavePreparation};

pub async fn get(
    user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Plan>, Error> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(crate::errors::internal_error)?;
    application_preparation::load_on(&mut connection, user.id, id)
        .await
        .map(Json)
}

pub async fn save(
    user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<SavePreparation>,
) -> Result<Json<Plan>, Error> {
    application_preparation::save(&pool, user.id, id, input)
        .await
        .map(Json)
}
