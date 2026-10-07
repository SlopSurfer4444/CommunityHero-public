//! Authenticated observation of one canonical post's durable media jobs.
use crate::*;
use axum::{Extension, extract::Query};
use crate::operator_auth::Actor;

pub(crate) async fn get(State(app):State<App>,Extension(_actor):Extension<Actor>,
    Path(post_id):Path<String>,Query(query):Query<HashMap<String,String>>)->ApiResult<Json<Value>> {
    let offset=offset(&query)?;
    Ok(Json(app.db.read_post_media_status(&post_id,app.account.display(),offset).await?))
}
fn offset(query:&HashMap<String,String>)->ApiResult<u32>{
    if query.keys().any(|key|key!="offset"){return Err(bad("Specify only offset"));}
    match query.get("offset") {
        None=>Ok(0),
        Some(value) if !value.is_empty()&&value.bytes().all(|byte|byte.is_ascii_digit())=>
            value.parse().map_err(|_|bad("Invalid media job offset")),
        _=>Err(bad("Invalid media job offset")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_status_offset_rejects_negative_overflow_and_unknown_fields(){
        assert_eq!(offset(&HashMap::new()).unwrap(),0);
        assert_eq!(offset(&HashMap::from([("offset".into(),"100".into())])).unwrap(),100);
        for value in ["","-1","+1","1.0"," 1","4294967296"] {
            assert!(offset(&HashMap::from([("offset".into(),value.into())])).is_err());
        }
        assert!(offset(&HashMap::from([("account".into(),"BAW Russia".into())])).is_err());
    }
}
