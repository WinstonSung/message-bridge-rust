use std::sync::Arc;

use proc_qq::re_exports::ricq::msg::MessageChain;
use proc_qq::re_exports::ricq_core::msg::elem;
use proc_qq::FileSessionStore;
use proc_qq::{Authentication, ClientBuilder, DeviceSource, ModuleEventHandler, ModuleEventProcess, ShowQR};
use tracing::{debug, error};

use crate::bridge_qq::handler::DefaultHandler;
use crate::{bridge, Config};
use bridge::pojo::BridgeMessagePO;

mod group_message_id;
mod handler;

use group_message_id::GroupMessageId;

type RqClient = proc_qq::re_exports::ricq::Client;

pub async fn upload_group_image(group_id: u64, url: &str, rq_client: Arc<RqClient>) -> anyhow::Result<elem::GroupImage> {
    let client = reqwest::Client::new();
    let stream = client.get(url).send().await?;
    let img_bytes = stream.bytes().await.unwrap();
    let group_image = rq_client.upload_group_image(group_id as i64, img_bytes.to_vec()).await?;
    Ok(group_image)
}

/// # 处理 at 消息
/// ## Argument
/// - `target` 被 at 用户
/// - `send_content` 同步消息链
async fn proc_at(target: &str, send_content: &mut MessageChain) {
    let bridge_user = bridge::manager::BRIDGE_USER_MANAGER.lock().await.get(target).await;
    if let None = bridge_user {
        send_content.push(elem::Text::new(format!("@[UN] {}", target)));
        return;
    }
    let bridge_user = bridge_user.unwrap();
    // 查看桥关联的本平台用户id
    if let Some(ref_user) = bridge_user.find_by_platform("QQ").await {
        if let Ok(origin_id) = ref_user.origin_id.parse::<i64>() {
            send_content.push(elem::At::new(origin_id));
            return;
        }
    }
    // 没有关联账号用标准格式发送消息
    send_content.push(elem::Text::new(format!("@{}", bridge_user.to_string())));
}

/**
 * 同步消息方法
 */
pub async fn sync_message(bridge: Arc<bridge::BridgeClient>, rq_client: Arc<RqClient>) {
    let mut subs = bridge.sender.subscribe();
    let bot_id = rq_client.uin().await;
    loop {
        let message = match subs.recv().await {
            Ok(m) => m,
            Err(err) => {
                error!(?err, "[{bot_id}] 消息同步失败");
                continue;
            }
        };

        let mut send_content = MessageChain::default();

        // 配置发送者头像
        if let Some(avatar_url) = &message.avatar_url {
            debug!("用户头像: {:?}", message.avatar_url);
            let image = upload_group_image(message.bridge_config.qqGroup, avatar_url, rq_client.clone()).await;
            if let Result::Ok(image) = image {
                send_content.push(image);
            }
        }
        let bridge_user = bridge::manager::BRIDGE_USER_MANAGER.lock().await.get(&message.sender_id).await;
        // 配置发送者用户名
        send_content.push(elem::Text::new(format!("{}\n", bridge_user.unwrap().to_string())));

        for chain in &message.message_chain {
            match chain {
                bridge::MessageContent::Reply { id } => {
                    if let Some(id) = id {
                        let reply_message = bridge::manager::BRIDGE_MESSAGE_MANAGER.lock().await.get(id).await;
                        if let Some(reply_message) = reply_message {
                            to_reply_content(&mut send_content, reply_message, bot_id).await
                        } else {
                            send_content.push(elem::Text::new("> {回复消息}\n".to_string()));
                        }
                    }
                }
                // 桥文本 转 qq文本
                bridge::MessageContent::Plain { text } => {
                    let mention_text_list = parse_text_mention_rule(text.to_string());
                    for mention_text in mention_text_list {
                        match mention_text {
                            MentionText::Text(text) => send_content.push(elem::Text::new(text)),
                            MentionText::MentionText { id, .. } => send_content.push(elem::At::new(id)),
                        }
                    }
                }
                // @桥用户 转 @qq用户 或 @文本
                bridge::MessageContent::At { id } => proc_at(id, &mut send_content).await,
                // 桥图片 转 qq图片
                bridge::MessageContent::Image(image) => {
                    debug!("桥消息-图片: {:?}", image);
                    match image.clone().load_data().await {
                        Ok(data) => match rq_client.upload_group_image(message.bridge_config.qqGroup as i64, data).await {
                            Ok(image) => {
                                send_content.push(image);
                            }
                            Err(_) => {}
                        },
                        Err(_) => {}
                    }
                }
                _ => send_content.push(elem::Text::new("{未处理的桥信息}".to_string())),
            }
        }
        debug!("[QQ] 同步消息");
        debug!("{:?}", send_content);
        debug!("{:?}", message.bridge_config.qqGroup as i64);

        // seqs: [6539], rands: [1442369605], time: 1678267174
        // rq_client.send_message(routing_head, message_chain, ptt);

        let result = rq_client
            .send_group_message(message.bridge_config.qqGroup as i64, send_content)
            .await
            .ok();
        if let Some(receipt) = result {
            // 发送成功后, 将平台消息和桥消息进行关联, 为以后进行回复功能
            let seqs = receipt.seqs.first().unwrap().clone();
            let group_message_id = GroupMessageId {
                group_id: message.bridge_config.qqGroup,
                seqs,
            };
            bridge::manager::BRIDGE_MESSAGE_MANAGER
                .lock()
                .await
                .ref_bridge_message(bridge::pojo::BridgeMessageRefMessageForm {
                    bridge_message_id: message.id,
                    platform: "QQ".to_string(),
                    origin_id: group_message_id.to_string(),
                })
                .await;
        }
    }
}

/**
 * 消息桥构建入口
 */
pub async fn start(config: Arc<Config>, bridge: Arc<bridge::BridgeClient>) {
    tracing::info!("[QQ] 初始化QQ桥");
    // 确认配置无误
    let auth = match config.qq_config.get_auth() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(?e);
            return;
        }
    };
    let version = match config.qq_config.get_version() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(?e);
            return;
        }
    };

    let handler = DefaultHandler {
        config: config.clone(),
        bridge: bridge.clone(),
        origin_client: None,
    };
    let handler = Box::new(handler);
    let on_message = ModuleEventHandler {
        name: "OnMessage".to_owned(),
        process: ModuleEventProcess::Message(handler),
    };

    // let modules = module!("qq_bridge", "qq桥模块", handler);
    let module = proc_qq::Module {
        id: "qq_bridge".to_string(),
        name: "qq桥模块".to_string(),
        handles: vec![on_message],
    };

    let mut show_qr = ShowQR::OpenBySystem;
    if let Authentication::QRCode = auth {
        if config.print_qr.unwrap_or_else(|| false) {
            show_qr = ShowQR::PrintToConsole;
        }
    }

    let client = ClientBuilder::new()
        .session_store(FileSessionStore::boxed("session.token"))
        .authentication(auth)
        .show_rq(show_qr)
        .device(DeviceSource::JsonFile("device.json".to_owned()))
        .version(version)
        .modules(vec![module])
        .build()
        .await
        .unwrap();
    let arc = Arc::new(client);
    tokio::select! {
        Err(e) = proc_qq::run_client(arc.clone()) => {
            tracing::error!(err = ?e, "[QQ] QQ客户端退出");
        },
        _ = sync_message(bridge.clone(), arc.rq_client.clone()) => {
            tracing::warn!("[QQ] QQ桥关闭");
        },
    }
}

/**
 * 申请桥用户
 */
async fn apply_bridge_user(id: u64, name: &str) -> bridge::user::BridgeUser {
    let bridge_user = bridge::manager::BRIDGE_USER_MANAGER
        .lock()
        .await
        .likeAndSave(bridge::pojo::BridgeUserSaveForm {
            origin_id: id.to_string(),
            platform: "QQ".to_string(),
            display_text: format!("{}({})", name, id),
        })
        .await;
    bridge_user.unwrap()
}

/**
 * 处理回复
 */
async fn to_reply_content(message_chain: &mut MessageChain, reply_message: BridgeMessagePO, uni: i64) {
    let refs = reply_message.refs.iter().find(|refs| refs.platform.eq("QQ"));
    let bridge_user = bridge::manager::BRIDGE_USER_MANAGER
        .lock()
        .await
        .get(&reply_message.sender_id)
        .await
        .unwrap();
    if let Some(refs) = refs {
        let group_message_id = GroupMessageId::from_bridge_message_id(&refs.origin_id);
        let mut reply_content = MessageChain::default();
        let sender: i64 = if bridge_user.platform == "QQ" {
            bridge_user.origin_id.parse::<i64>().unwrap()
        } else {
            uni
        };
        for chain in reply_message.message_chain {
            match chain {
                bridge::MessageContent::Reply { .. } => {
                    reply_content.push(elem::Text::new("[回复消息]".to_string()));
                }
                bridge::MessageContent::Plain { text } => reply_content.push(elem::Text::new(text.to_string())),
                bridge::MessageContent::At { id } => proc_at(&id, &mut reply_content).await,
                bridge::MessageContent::Image(..) => reply_content.push(elem::Text::new("[图片]".to_string())),
                _ => {}
            }
        }
        let reply = elem::Reply {
            reply_seq: group_message_id.seqs,
            sender,
            time: 0,
            elements: reply_content,
        };
        message_chain.with_reply(reply);
    } else {
        message_chain.push(elem::Text::new("{回复消息}".to_string()));
    }
    // let mut reply_content = MessageChain::default();
    // reply_content.push(elem::Text::new("test custom reply3".to_string()));
    // let reply = elem::Reply {
    //     reply_seq: 6539,
    //     sender: 243249439,
    //     time: 1678267174,
    //     elements: reply_content,
    // };
    // send_content.with_reply(reply);
    // send_content.0.push(ricq_core::pb::msg::elem::Elem::SrcMsg(
    //     ricq_core::pb::msg::SourceMsg {
    //         orig_seqs: vec![6539],
    //         sender_uin: Some(243249439),
    //         time: Some(1678267174),
    //         flag: Some(1),
    //         elems: reply_content.into(),
    //         rich_msg: Some(vec![]),
    //         pb_reserve: Some(vec![]),
    //         src_msg: Some(vec![]),
    //         troop_name: Some(vec![]),
    //         ..Default::default()
    //     },
    // ));
}

/**
 * 解析文本规则取出提及@[DC]用户的文本
 */
#[derive(Debug)]
pub enum MentionText {
    Text(String),
    MentionText { name: String, id: i64 },
}
pub fn parse_text_mention_rule(text: String) -> Vec<MentionText> {
    let mut text = text;
    let mut chain: Vec<MentionText> = vec![];
    let split_const = "#|x-x|#".to_string();
    let reg_at_user = regex::Regex::new(r"@\[QQ\] ([^\n^@]+)\(([0-9]+)\)").unwrap();
    while let Some(caps) = reg_at_user.captures(text.as_str()) {
        println!("{:?}", caps);
        let from = caps.get(0).unwrap().as_str();
        let name = caps.get(1).unwrap().as_str().to_string();
        let id = caps.get(2).unwrap().as_str().parse::<i64>().unwrap();

        let result = text.replace(from, &split_const);
        let splits: Vec<&str> = result.split(&split_const).collect();
        let prefix = splits.get(0).unwrap();
        chain.push(MentionText::Text(prefix.to_string()));
        chain.push(MentionText::MentionText { name, id });
        if let Some(fix) = splits.get(1) {
            text = fix.to_string();
        }
    }
    if text.len() > 0 {
        chain.push(MentionText::Text(text.to_string()));
    }
    println!("parse_text_mention_rule: {:?}", chain);
    chain
}
