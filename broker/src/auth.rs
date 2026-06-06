#[async_trait::async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, username: &str, password: &str) -> bool;
}

pub struct AllowAllAuth;

#[async_trait::async_trait]
impl AuthProvider for AllowAllAuth {
    async fn authenticate(&self, _username: &str, _password: &str) -> bool {
        true
    }
}

pub struct FileAuth {
    credentials: Vec<(String, String)>,
}

impl FileAuth {
    pub fn new(credentials: Vec<(String, String)>) -> Self {
        Self { credentials }
    }

    pub fn from_file(path: &str) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        let credentials = content
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let mut parts = line.splitn(2, ':');
                match (parts.next(), parts.next()) {
                    (Some(user), Some(pass)) => Some((user.to_string(), pass.to_string())),
                    _ => None,
                }
            })
            .collect();
        Ok(Self { credentials })
    }
}

#[async_trait::async_trait]
impl AuthProvider for FileAuth {
    async fn authenticate(&self, username: &str, password: &str) -> bool {
        self.credentials
            .iter()
            .any(|(u, p)| u == username && p == password)
    }
}
