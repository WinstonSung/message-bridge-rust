#![allow(non_snake_case)]

use lazy_static::lazy_static;
use std::fs;
use tokio::sync::Mutex;

use crate::bridge::pojo::BridgeUserSaveForm;
use crate::bridge::user::BridgeUser;
use crate::elo;

pub struct BridgeUserManager {
    bridge_users: Vec<BridgeUser>,
}

impl BridgeUserManager {
    pub fn new() -> BridgeUserManager {
        let path = "./data/bridge_user.json";
        if let Ok(true) = fs::try_exists(path) {
            let file = fs::read_to_string(path).unwrap();
            let bridge_users: Vec<BridgeUser> = serde_json::from_str(file.as_str()).unwrap();
            return BridgeUserManager { bridge_users };
        }
        BridgeUserManager {
            bridge_users: vec![],
        }
    }

    /// 根据id查询指定用户
    pub async fn get(&self, id: &str) -> Option<BridgeUser> {
        for user in &self.bridge_users {
            if id.eq(&user.id) {
                return Some(user.clone());
            }
        }
        None
    }

    /// 模糊查询用户 (源id和平台)
    pub async fn like(&self, origin_id: &str, platform: &str) -> Option<BridgeUser> {
        for user in &self.bridge_users {
            if origin_id.eq(&user.origin_id) && platform.eq(&user.platform) {
                return Some(user.clone());
            }
        }
        None
    }

    pub async fn likeAndSave(&mut self, form: BridgeUserSaveForm) -> Result<BridgeUser, String> {
        match self.like(&form.origin_id, &form.platform).await {
            Some(user) => Ok(user),
            None => self.save(form).await,
        }
    }

    /// 通过关联id和平台查询绑定的另一个账号
    pub async fn findByRefAndPlatform(&self, ref_id: &str, platform: &str) -> Option<BridgeUser> {
        for user in &self.bridge_users {
            if let None = user.ref_id {
                return None;
            }
            if ref_id.eq(user.ref_id.as_ref().unwrap()) && platform.eq(&user.platform) {
                return Some(user.clone());
            }
        }
        None
    }

    /// 保存一条新的用户
    pub async fn save(&mut self, form: BridgeUserSaveForm) -> Result<BridgeUser, String> {
        if self.like(&form.origin_id, &form.platform).await.is_some() {
            let help = format!(
                "该平台{:}已存在用户id为{:}的用户",
                &form.platform, &form.origin_id
            );
            tracing::info!("{help}");
            return Err(help);
        }
        let user = BridgeUser {
            id: uuid::Uuid::new_v4().to_string(),
            origin_id: form.origin_id,
            platform: form.platform,
            display_text: form.display_text,
            ref_id: None,
        };
        self.bridge_users.push(user.clone());
        self.serialize();
        Ok(user)
    }

    /// # 批量保存的用户
    /// ### Argument
    /// `queue` 待更新队列
    /// ### Returns
    /// - `Ok(..)` 更新行数
    /// - `Err(..)` 失败描述
    pub async fn batch_update(&mut self, queue: &[BridgeUser]) -> Result<usize, String> {
        let mut count = 0;
        for item in queue {
            let old = elo! {
                self.bridge_users.iter_mut().find(|t| t.id == item.id)
                ;;// else
                continue
            };
            old.display_text = item.display_text.clone();
            old.origin_id = item.origin_id.clone();
            old.platform = item.platform.clone();
            old.ref_id = item.ref_id.clone();
            count += 1;
        }
        self.serialize();
        Ok(count)
    }

    fn serialize(&self) {
        let content = serde_json::to_string(&self.bridge_users).unwrap();
        fs::write("./data/bridge_user.json", content).unwrap();
    }
}

lazy_static! {
    // static ref VEC:Vec<u8> = vec![0x18u8, 0x11u8];
    // static ref MAP: HashMap<u32, String> = {
    //     let mut map = HashMap::new();
    //     map.insert(18, "hury".to_owned());s
    //     map
    // };
    // static ref PAGE:u32 = mulit(18);

    pub static ref BRIDGE_USER_MANAGER: Mutex<BridgeUserManager> = Mutex::new(BridgeUserManager::new());
}
