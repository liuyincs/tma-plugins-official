//! 查询构造夹具（与原 `builtin_tests::support` 同名同形状）。

use crate::{EntityQuery, FetchedId, ScrapeEntityKind};

pub fn artist_query(mbid: Option<&str>, name: Option<&str>) -> EntityQuery {
    EntityQuery {
        kind: ScrapeEntityKind::Artist,
        mbid: mbid.map(str::to_string),
        isrc: None,
        name: name.map(str::to_string),
        artist_name: None,
        known_external_ids: Vec::new(),
        library_types: Vec::new(),
    }
}

pub fn artist_query_with_known(
    mbid: Option<&str>,
    name: Option<&str>,
    known: &[(&str, &str)],
) -> EntityQuery {
    EntityQuery {
        known_external_ids: known
            .iter()
            .map(|(p, id)| FetchedId {
                provider: p.to_string(),
                external_id: id.to_string(),
                url: None,
            })
            .collect(),
        ..artist_query(mbid, name)
    }
}

pub const BEATLES_MBID: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d260c2";
pub const ABBEY_RG_MBID: &str = "b588555c-e1a7-45b1-8f91-2048a8393b58";
pub const ABBEY_RELEASE_MBID: &str = "22222222-2222-2222-2222-222222222222";
