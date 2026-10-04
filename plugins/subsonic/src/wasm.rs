#![cfg(target_arch = "wasm32")]

use serde_json::{Value, json};
use tma_plugin_sdk::{
    CAPABILITY_DTO_VERSION, CatalogReadRequest, CatalogReadResponse, IdentityReadRequest,
    IdentityReadResponse, MediaStreamRequest, PluginError, PluginHttpRequest, PluginHttpResponse,
};

use crate::protocol::{endpoint_name, param, parse_range, protocol_error, validate_protocol};
use crate::response::{
    album_json, album_xml, attr_opt, error_response, esc, media_response, numeric_param,
    ok_response, required_param, song_json, song_xml,
};

// FFI 样板（tma_manifest / tma_http 导出、三条 capability 宿主函数声明与
// 各自的安全助手 catalog / identity / media_stream）由宏吐出。
tma_plugin_sdk::plugin! {
    manifest = "../manifest.json",
    slug = "subsonic",
    http = run_http,
    catalog_read = catalog,
    identity_read = identity,
    media_stream = media_stream,
}

/// 业务错误一律折成 Subsonic failed 响应返回（HTTP 200 + error 元素），
/// 让客户端按协议展示；`run_http` 自身只有不可恢复时才 Err。
fn run_http(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    handle(request.clone()).or_else(|error| Ok(error_response(&request, error)))
}

fn handle(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    validate_protocol(&request)?;
    let endpoint = endpoint_name(&request.path);

    if endpoint != "ping" {
        let identity = current_identity()?;
        if !identity.ok {
            return Err(protocol_error(50, "未认证"));
        }
    }

    match endpoint {
        "ping" => Ok(ok_response(&request, None, "<ping version=\"1.16.1\" />")),
        "getUser" => user(request),
        "getMusicFolders" => folders(request),
        "getArtists" => artists(request),
        "getArtist" => artist_detail(request),
        "getAlbumList" => album_list(request, false),
        "getAlbumList2" => album_list(request, true),
        "getAlbum" => album(request),
        "getSong" => song(request),
        "stream" => stream(request),
        "getCoverArt" => cover_art(request),
        "star" | "unstar" | "setRating" | "scrobble" | "search3" | "getPlaylists" => {
            Err(protocol_error(0, "该 Subsonic 端点暂不支持"))
        }
        _ => Err(protocol_error(0, "未知或暂不支持的 Subsonic 端点")),
    }
}

/// 当前调用身份（宿主在路由前已完成认证，ok=false 即视为未认证）。
fn current_identity() -> Result<IdentityReadResponse, PluginError> {
    identity(IdentityReadRequest {
        version: CAPABILITY_DTO_VERSION,
    })
}

/// `getUser`：登录流程的身份回显。角色按宿主能力保守映射——只读适配器
/// 不给写角色；adminRole 对应宿主 is_admin（部分客户端用它放行设置页）。
fn user(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let identity = current_identity()?;
    let username = identity.username.clone().unwrap_or_default();
    let roles = json!({
        "adminRole": identity.is_admin,
        "settingsRole": identity.is_admin,
        "downloadRole": true,
        "uploadRole": false,
        "playlistRole": true,
        "coverArtRole": true,
        "commentRole": false,
        "podcastRole": false,
        "streamRole": true,
        "jukeboxRole": false,
        "shareRole": false,
        "videoConversionRole": false,
    });
    let mut roles_obj = roles.as_object().unwrap().clone();
    roles_obj.insert("username".into(), json!(username));
    let xml = format!(
        "<user username=\"{}\" adminRole=\"{}\" settingsRole=\"{}\" downloadRole=\"true\" uploadRole=\"false\" playlistRole=\"true\" coverArtRole=\"true\" commentRole=\"false\" podcastRole=\"false\" streamRole=\"true\" jukeboxRole=\"false\" shareRole=\"false\" videoConversionRole=\"false\" />",
        esc(&username),
        identity.is_admin,
        identity.is_admin,
    );
    Ok(ok_response(
        &request,
        Some(json!({"user": roles_obj})),
        &xml,
    ))
}

fn folders(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let response = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("library".into()),
        limit: 1000,
        ..Default::default()
    })?;
    let items = response.items;
    let json_body = json!({
        "musicFolders": { "musicFolder": items.iter().map(|item| json!({"id": item.id, "name": item.title})).collect::<Vec<_>>() }
    });
    let xml = format!(
        "<musicFolders>{}</musicFolders>",
        items
            .iter()
            .map(|item| format!(
                "<musicFolder id=\"{}\" name=\"{}\" />",
                esc(&item.id),
                esc(&item.title)
            ))
            .collect::<String>()
    );
    Ok(ok_response(&request, Some(json_body), &xml))
}

fn artists(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let count = numeric_param(&request, "count", 20, 500)?;
    let offset = numeric_param(&request, "offset", 0, u32::MAX)?;
    if param(&request, "musicFolderId").is_some() {
        return Err(protocol_error(0, "暂不支持 musicFolderId 过滤"));
    }
    if param(&request, "sortBy").is_some_and(|value| value != "name")
        || param(&request, "order").is_some_and(|value| value != "asc")
    {
        return Err(protocol_error(0, "仅支持按名称升序获取艺术家"));
    }
    let response = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("artist".into()),
        query: param(&request, "name").or_else(|| param(&request, "query")),
        cursor: (offset > 0).then(|| offset.to_string()),
        limit: count,
        ..Default::default()
    })?;
    let json_artists: Vec<Value> = response
        .items
        .iter()
        .map(|item| json!({"id": item.id, "name": item.title}))
        .collect();
    let json_body = json!({"artists": {"index": [{"name": "", "artist": json_artists}]}});
    let xml = format!(
        "<artists><index name=\"\">{}</index></artists>",
        response
            .items
            .iter()
            .map(|item| format!(
                "<artist id=\"{}\" name=\"{}\" />",
                esc(&item.id),
                esc(&item.title)
            ))
            .collect::<String>()
    );
    Ok(ok_response(&request, Some(json_body), &xml))
}

fn album(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let id = required_param(&request, "id")?;
    let response = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("album".into()),
        id: Some(id.clone()),
        limit: 1,
        ..Default::default()
    })?;
    let Some(item) = response.items.first() else {
        return Err(protocol_error(70, "专辑不存在"));
    };
    let tracks = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("song".into()),
        parent_id: Some(id.clone()),
        limit: 1000,
        ..Default::default()
    })?;
    let json_body = json!({"album": {"id": item.id, "name": item.title, "artist": item.artist, "artistId": item.parent_id, "year": item.year, "songCount": tracks.items.len(), "song": tracks.items.iter().map(song_json).collect::<Vec<_>>()}});
    let xml = format!(
        "<album id=\"{}\" name=\"{}\"{} songCount=\"{}\">{}</album>",
        esc(&item.id),
        esc(&item.title),
        attr_opt("artist", item.artist.as_deref()),
        tracks.items.len(),
        tracks.items.iter().map(song_xml).collect::<String>()
    );
    Ok(ok_response(&request, Some(json_body), &xml))
}

/// `getArtist`：艺术家详情 + 名下专辑列表（宿主按 parent_id=artist 过滤专辑）。
fn artist_detail(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let id = required_param(&request, "id")?;
    let response = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("artist".into()),
        id: Some(id.clone()),
        limit: 1,
        ..Default::default()
    })?;
    let Some(item) = response.items.first() else {
        return Err(protocol_error(70, "艺术家不存在"));
    };
    let albums = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("album".into()),
        parent_id: Some(id.clone()),
        limit: 500,
        ..Default::default()
    })?;
    let json_body = json!({
        "artist": {
            "id": item.id,
            "name": item.title,
            "albumCount": albums.items.len(),
            "album": albums.items.iter().map(album_json).collect::<Vec<_>>(),
        }
    });
    let xml = format!(
        "<artist id=\"{}\" name=\"{}\" albumCount=\"{}\">{}</artist>",
        esc(&item.id),
        esc(&item.title),
        albums.items.len(),
        albums.items.iter().map(album_xml).collect::<String>(),
    );
    Ok(ok_response(&request, Some(json_body), &xml))
}

/// `getAlbumList`/`getAlbumList2`：专辑列表。宿主目录只按名称排序，
/// `type` 各取值统一返回该顺序；过滤类参数（年份/流派/文件夹）宿主不支持，
/// 明示拒绝而不是静默返回未过滤的数据。
fn album_list(request: PluginHttpRequest, v2: bool) -> Result<PluginHttpResponse, PluginError> {
    let size = numeric_param(&request, "size", 10, 500)?;
    let offset = numeric_param(&request, "offset", 0, u32::MAX)?;
    for name in ["musicFolderId", "genre", "fromYear", "toYear"] {
        if param(&request, name).is_some() {
            return Err(protocol_error(0, &format!("暂不支持 {name} 过滤")));
        }
    }
    let response = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("album".into()),
        cursor: (offset > 0).then(|| offset.to_string()),
        limit: size,
        ..Default::default()
    })?;
    let key = if v2 { "albumList2" } else { "albumList" };
    let json_body = json!({
        key: {"album": response.items.iter().map(album_json).collect::<Vec<_>>()},
    });
    let xml = format!(
        "<{key}>{}</{key}>",
        response.items.iter().map(album_xml).collect::<String>(),
    );
    Ok(ok_response(&request, Some(json_body), &xml))
}

fn song(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let id = required_param(&request, "id")?;
    let response = read_catalog(CatalogReadRequest {
        version: CAPABILITY_DTO_VERSION,
        kind: Some("song".into()),
        id: Some(id),
        limit: 1,
        ..Default::default()
    })?;
    let Some(item) = response.items.first() else {
        return Err(protocol_error(70, "歌曲不存在"));
    };
    let json_body = json!({"song": song_json(item)});
    let xml = song_xml(item);
    Ok(ok_response(&request, Some(json_body), &xml))
}

fn stream(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let id = required_param(&request, "id")?;
    let range = request
        .headers
        .get("range")
        .map(|value| parse_range(value))
        .transpose()?;
    let codec = param(&request, "format");
    let bitrate_kbps = param(&request, "maxBitRate")
        .map(|value| {
            value
                .parse()
                .map_err(|_| protocol_error(10, "maxBitRate 必须是整数"))
        })
        .transpose()?;
    let response = media_stream(MediaStreamRequest {
        version: CAPABILITY_DTO_VERSION,
        media_id: id,
        codec,
        bitrate_kbps,
        range,
        client_capabilities: None,
    })?;
    media_response(response)
}

fn cover_art(request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    let id = required_param(&request, "id")?;
    if param(&request, "size").is_some() {
        return Err(protocol_error(0, "暂不支持按 size 缩放封面"));
    }
    let media_id =
        if id.starts_with("cover:") || id.starts_with("track:") || id.starts_with("artist:") {
            id
        } else {
            format!("cover:{id}")
        };
    let response = media_stream(MediaStreamRequest {
        version: CAPABILITY_DTO_VERSION,
        media_id,
        codec: None,
        bitrate_kbps: None,
        range: None,
        client_capabilities: None,
    })?;
    media_response(response)
}

/// catalog 读取 = 宏生成的 `catalog` 宿主函数助手 + `ok=false` 折叠为协议错误。
fn read_catalog(request: CatalogReadRequest) -> Result<CatalogReadResponse, PluginError> {
    let response = catalog(request)?;
    if !response.ok {
        return Err(protocol_error(
            0,
            response
                .error
                .as_ref()
                .map(|e| e.message.as_str())
                .unwrap_or("catalog 读取失败"),
        ));
    }
    Ok(response)
}
