use super::schema::Database;

#[derive(Debug, Clone)]
pub struct Mission {
    pub key: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub created_by: Option<String>,
    pub metadata: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Database {
    /// Generate the next mission key: MIS-{5-digit}
    fn next_mission_key(&self) -> Result<String, String> {
        let conn = self.conn.lock().unwrap();
        let max: Option<String> = conn
            .query_row(
                "SELECT key FROM missions ORDER BY key DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();

        let next_num = if let Some(last_key) = max {
            let num_part = &last_key[4..]; // skip "MIS-"
            num_part.parse::<u32>().unwrap_or(0) + 1
        } else {
            1
        };
        Ok(format!("MIS-{:05}", next_num))
    }

    /// Create a new mission with auto-generated key.
    pub fn create_mission(
        &self,
        title: &str,
        description: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<String, String> {
        let key = self.next_mission_key()?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO missions (key, title, description, created_by) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![key, title, description, created_by],
        )
        .map_err(|e| format!("Failed to create mission: {}", e))?;
        Ok(key)
    }

    /// Update mission fields dynamically.
    pub fn update_mission(
        &self,
        key: &str,
        status: Option<&str>,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), String> {
        let mut sets = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(v) = status {
            sets.push("status = ?");
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = title {
            sets.push("title = ?");
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = description {
            sets.push("description = ?");
            params.push(Box::new(v.to_string()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        sets.push("updated_at = CURRENT_TIMESTAMP");

        let numbered_sets: Vec<String> = sets
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if s.contains('?') {
                    s.replace('?', &format!("?{}", i + 1))
                } else {
                    s.to_string()
                }
            })
            .collect();

        params.push(Box::new(key.to_string()));
        let key_idx = params.len();

        let sql = format!(
            "UPDATE missions SET {} WHERE key = ?{}",
            numbered_sets.join(", "),
            key_idx
        );

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let conn = self.conn.lock().unwrap();
        conn.execute(&sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to update mission: {}", e))?;
        Ok(())
    }

    /// Get a single mission by key.
    pub fn get_mission(&self, key: &str) -> Result<Option<Mission>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT key, title, description, status, created_by, metadata, created_at, updated_at \
             FROM missions WHERE key = ?1",
            [key],
            Self::map_mission_row,
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            _ => Err(format!("Failed to get mission: {}", e)),
        })
    }

    /// Delete a mission by key.
    pub fn delete_mission(&self, key: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute("DELETE FROM missions WHERE key = ?1", [key])
            .map_err(|e| format!("Failed to delete mission: {}", e))?;
        if rows == 0 {
            return Err(format!("Mission not found: {}", key));
        }
        Ok(())
    }

    /// List missions with optional status filter.
    pub fn list_missions(&self, status: Option<&str>) -> Result<Vec<Mission>, String> {
        let conn = self.conn.lock().unwrap();

        let sql = if status.is_some() {
            "SELECT key, title, description, status, created_by, metadata, created_at, updated_at \
             FROM missions WHERE status = ?1 ORDER BY created_at"
        } else {
            "SELECT key, title, description, status, created_by, metadata, created_at, updated_at \
             FROM missions ORDER BY created_at"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare list_missions: {}", e))?;

        let rows = if let Some(s) = status {
            stmt.query_map([s], Self::map_mission_row)
        } else {
            stmt.query_map([], Self::map_mission_row)
        }
        .map_err(|e| format!("Failed to query missions: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn map_mission_row(row: &rusqlite::Row) -> rusqlite::Result<Mission> {
        Ok(Mission {
            key: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            status: row.get(3)?,
            created_by: row.get(4)?,
            metadata: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_mission() {
        let db = setup();
        let key = db.create_mission("Mission Alpha", Some("First mission"), Some("ino")).unwrap();
        assert_eq!(key, "MIS-00001");

        let key2 = db.create_mission("Mission Bravo", None, None).unwrap();
        assert_eq!(key2, "MIS-00002");
    }

    #[test]
    fn test_list_missions() {
        let db = setup();
        db.create_mission("M1", None, None).unwrap();
        db.create_mission("M2", None, None).unwrap();

        let all = db.list_missions(None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].title, "M1");
        assert_eq!(all[1].title, "M2");
    }

    #[test]
    fn test_list_missions_filter_status() {
        let db = setup();
        let k1 = db.create_mission("Active", None, None).unwrap();
        let k2 = db.create_mission("Done", None, None).unwrap();
        db.update_mission(&k2, Some("completed"), None, None).unwrap();

        let active = db.list_missions(Some("active")).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key, k1);

        let completed = db.list_missions(Some("completed")).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].key, k2);
    }

    #[test]
    fn test_update_mission() {
        let db = setup();
        let key = db.create_mission("Original", None, None).unwrap();
        db.update_mission(&key, Some("paused"), Some("Renamed"), Some("New desc"))
            .unwrap();

        let missions = db.list_missions(None).unwrap();
        assert_eq!(missions[0].title, "Renamed");
        assert_eq!(missions[0].status, "paused");
        assert_eq!(missions[0].description.as_deref(), Some("New desc"));
    }

    #[test]
    fn test_update_mission_no_fields() {
        let db = setup();
        let key = db.create_mission("Test", None, None).unwrap();
        // Should be a no-op, not an error
        db.update_mission(&key, None, None, None).unwrap();
    }
}
