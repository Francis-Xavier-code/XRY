//! 学分管理数据库（SQLite，`HILIA_HOME/data/credit.db`）。
//!
//! 面向大学辅导员（管理员）与学生：
//! - classes：班级
//! - students：学生（含 QQ/企业微信/飞书 平台绑定列，用于识别提问者身份）
//! - credit_types：学分类型
//! - credit_records：学分记录（正分加分、负分扣分，按学期/类型汇总）
//!
//! 所有读写走本模块，不做手改文件；CSV 导入格式固定为「学号,姓名,班级,性别,电话」。

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClassRow {
    pub id: i64,
    pub name: String,
    pub grade: String,
    pub major: String,
    pub note: String,
    pub created_at: String,
    pub student_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StudentRow {
    pub id: i64,
    pub student_no: String,
    pub name: String,
    pub class_id: Option<i64>,
    pub class_name: Option<String>,
    pub gender: String,
    pub phone: String,
    pub qq_id: String,
    pub wecom_id: String,
    pub feishu_id: String,
    pub note: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CreditTypeRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub max_points: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CreditRecordRow {
    pub id: i64,
    pub student_id: i64,
    pub student_no: String,
    pub student_name: String,
    pub class_name: Option<String>,
    pub type_id: Option<i64>,
    pub type_name: Option<String>,
    pub points: f64,
    pub semester: String,
    pub happened_on: String,
    pub note: String,
    pub operator: String,
    pub created_at: String,
}

/// 学分汇总：某学生/班级按类型的得分。
#[derive(Debug, Clone)]
pub struct CreditSummary {
    pub by_type: Vec<(String, f64)>,
    pub total: f64,
}

/// 班级职位（班长/学委/团支书…，绑定学生 + 班级）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RoleRow {
    pub id: i64,
    pub class_id: i64,
    pub class_name: String,
    pub title: String,
    pub student_id: Option<i64>,
    pub student_no: Option<String>,
    pub student_name: Option<String>,
    pub note: String,
    pub created_at: String,
}

/// 学分申报（APK 问卷提交，导员审批）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubmissionRow {
    pub id: i64,
    pub student_id: i64,
    pub student_no: String,
    pub student_name: String,
    pub class_id: Option<i64>,
    pub class_name: Option<String>,
    pub type_id: Option<i64>,
    pub type_name: Option<String>,
    pub points: f64,
    pub description: String,
    pub evidence: String,
    pub status: String,
    pub device_id: String,
    pub review_note: String,
    pub reviewed_at: String,
    pub created_at: String,
}

/// 申报统计（按班级/类型聚合，供 AI 总结与面板展示）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubmissionSummaryRow {
    pub class_id: Option<i64>,
    pub class_name: Option<String>,
    pub type_name: Option<String>,
    pub pending: i64,
    pub approved: i64,
    pub rejected: i64,
    pub total_points: f64,
}

pub struct CreditsDb {
    conn: Mutex<Connection>,
}

impl CreditsDb {
    pub fn open(data_dir: &PathBuf) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("credit.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("opening credit database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", "5000")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        init_schema(&conn)?;
        seed_default_types(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening credit database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", "5000")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        init_schema(&conn)?;
        seed_default_types(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ─────────────────────────── 班级 ───────────────────────────

    pub fn add_class(&self, name: &str, grade: &str, major: &str, note: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("班级名称不能为空");
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO classes (name, grade, major, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, grade.trim(), major.trim(), note.trim(), now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_class(
        &self,
        id: i64,
        name: Option<&str>,
        grade: Option<&str>,
        major: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        if let Some(name) = name {
            let name = name.trim();
            if name.is_empty() {
                bail!("班级名称不能为空");
            }
            conn.execute(
                "UPDATE classes SET name = ?1 WHERE id = ?2",
                params![name, id],
            )?;
        }
        if let Some(grade) = grade {
            conn.execute(
                "UPDATE classes SET grade = ?1 WHERE id = ?2",
                params![grade.trim(), id],
            )?;
        }
        if let Some(major) = major {
            conn.execute(
                "UPDATE classes SET major = ?1 WHERE id = ?2",
                params![major.trim(), id],
            )?;
        }
        if let Some(note) = note {
            conn.execute(
                "UPDATE classes SET note = ?1 WHERE id = ?2",
                params![note.trim(), id],
            )?;
        }
        Ok(())
    }

    /// 删除班级：学生保留但解除班级关联，返回受影响人数。
    pub fn delete_class(&self, id: i64) -> Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let affected = tx.execute(
            "UPDATE students SET class_id = NULL WHERE class_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM classes WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(affected as i64)
    }

    pub fn list_classes(&self) -> Result<Vec<ClassRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, c.grade, c.major, c.note, c.created_at,
                    (SELECT COUNT(*) FROM students s WHERE s.class_id = c.id) AS student_count
             FROM classes c ORDER BY c.name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ClassRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    grade: row.get(2)?,
                    major: row.get(3)?,
                    note: row.get(4)?,
                    created_at: row.get(5)?,
                    student_count: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_class_by_name(&self, name: &str) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name FROM classes WHERE name = ?1",
            params![name.trim()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    // ─────────────────────────── 学生 ───────────────────────────

    pub fn add_student(
        &self,
        student_no: &str,
        name: &str,
        class_id: Option<i64>,
        gender: &str,
        phone: &str,
        qq_id: &str,
        wecom_id: &str,
        feishu_id: &str,
        note: &str,
    ) -> Result<i64> {
        let student_no = student_no.trim();
        let name = name.trim();
        if student_no.is_empty() {
            bail!("学号不能为空");
        }
        if name.is_empty() {
            bail!("姓名不能为空");
        }
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM students WHERE student_no = ?1)",
                params![student_no],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            bail!("学号 {student_no} 已存在");
        }
        conn.execute(
            "INSERT INTO students
               (student_no, name, class_id, gender, phone, qq_id, wecom_id, feishu_id, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                student_no,
                name,
                class_id,
                gender.trim(),
                phone.trim(),
                qq_id.trim(),
                wecom_id.trim(),
                feishu_id.trim(),
                note.trim(),
                now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_student(
        &self,
        id: i64,
        student_no: Option<&str>,
        name: Option<&str>,
        class_id: Option<Option<i64>>,
        gender: Option<&str>,
        phone: Option<&str>,
        qq_id: Option<&str>,
        wecom_id: Option<&str>,
        feishu_id: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut sets: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(v) = student_no {
            let v = v.trim();
            if v.is_empty() {
                bail!("学号不能为空");
            }
            sets.push("student_no = ?".into());
            values.push(text_value(v));
        }
        if let Some(v) = name {
            let v = v.trim();
            if v.is_empty() {
                bail!("姓名不能为空");
            }
            sets.push("name = ?".into());
            values.push(text_value(v));
        }
        if let Some(v) = class_id {
            sets.push("class_id = ?".into());
            values.push(match v {
                Some(id) => rusqlite::types::Value::Integer(id),
                None => rusqlite::types::Value::Null,
            });
        }
        if let Some(v) = gender {
            sets.push("gender = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = phone {
            sets.push("phone = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = qq_id {
            sets.push("qq_id = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = wecom_id {
            sets.push("wecom_id = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = feishu_id {
            sets.push("feishu_id = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = note {
            sets.push("note = ?".into());
            values.push(text_value(v.trim()));
        }
        if sets.is_empty() {
            return Ok(());
        }
        let sql = format!("UPDATE students SET {} WHERE id = ?", sets.join(", "));
        values.push(rusqlite::types::Value::Integer(id));
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        stmt.execute(params.as_slice())?;
        Ok(())
    }

    /// 删除学生：级联删除其学分记录，返回删除的记录数。
    pub fn delete_student(&self, id: i64) -> Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let affected = tx.execute(
            "DELETE FROM credit_records WHERE student_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM students WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(affected as i64)
    }

    pub fn find_student_by_id(&self, id: i64) -> Result<Option<StudentRow>> {
        let conn = self.conn.lock().unwrap();
        query_student_row(&conn, "WHERE s.id = ?1", params![id])
    }

    pub fn find_student_by_no(&self, student_no: &str) -> Result<Option<StudentRow>> {
        let conn = self.conn.lock().unwrap();
        query_student_row(&conn, "WHERE s.student_no = ?1", params![student_no.trim()])
    }

    /// 按平台绑定查找学生（QQ 号 / 企业微信 userid / 飞书 open_id）。
    pub fn find_student_by_platform(
        &self,
        platform: &str,
        platform_id: &str,
    ) -> Result<Option<StudentRow>> {
        if platform_id.trim().is_empty() {
            return Ok(None);
        }
        let column = match platform {
            "qq" => "s.qq_id",
            "wecom" => "s.wecom_id",
            "feishu" => "s.feishu_id",
            "apk" => "s.apk_id",
            _ => return Ok(None),
        };
        let conn = self.conn.lock().unwrap();
        let sql = format!("WHERE {column} = ?1");
        query_student_row(&conn, &sql, params![platform_id.trim()])
    }

    /// 按班级/关键词查询学生列表。
    pub fn query_students(&self, class_id: Option<i64>, keyword: &str) -> Result<Vec<StudentRow>> {
        let conn = self.conn.lock().unwrap();
        let keyword = keyword.trim();
        let mut sql = String::from(
            "SELECT s.id, s.student_no, s.name, s.class_id, c.name AS class_name,
                    s.gender, s.phone, s.qq_id, s.wecom_id, s.feishu_id, s.note, s.created_at
             FROM students s LEFT JOIN classes c ON c.id = s.class_id",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(cid) = class_id {
            conds.push("s.class_id = ?".to_string());
            values.push(rusqlite::types::Value::Integer(cid));
        }
        if !keyword.is_empty() {
            conds.push("(s.student_no LIKE ? OR s.name LIKE ?)".to_string());
            let pattern = format!("%{keyword}%");
            values.push(text_value(&pattern));
            values.push(text_value(&pattern));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY s.student_no");
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(params.as_slice(), student_row_mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ─────────────────────────── 学分类型 ───────────────────────────

    pub fn add_credit_type(&self, name: &str, description: &str, max_points: f64) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("学分类型名称不能为空");
        }
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credit_types WHERE name = ?1)",
                params![name],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            bail!("学分类型「{name}」已存在");
        }
        conn.execute(
            "INSERT INTO credit_types (name, description, max_points) VALUES (?1, ?2, ?3)",
            params![name, description.trim(), max_points],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_credit_types(&self) -> Result<Vec<CreditTypeRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, name, description, max_points FROM credit_types ORDER BY id")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CreditTypeRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    max_points: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_credit_type_by_name(&self, name: &str) -> Result<Option<(i64, String, f64)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, max_points FROM credit_types WHERE name = ?1",
            params![name.trim()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    // ─────────────────────────── 学分记录 ───────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn add_credit(
        &self,
        student_id: i64,
        type_id: Option<i64>,
        points: f64,
        semester: &str,
        happened_on: &str,
        note: &str,
        operator: &str,
    ) -> Result<i64> {
        if points == 0.0 {
            bail!("分值不能为 0（加分用正数，扣分用负数）");
        }
        if let Some(type_id) = type_id {
            if let Some((_, _, max)) = self.find_credit_type_by_id(type_id)? {
                if max > 0.0 && points > 0.0 {
                    // 检查该类型总分是否超上限（含本条）
                    let total = self.summary_by_student_type(student_id, type_id)?;
                    if total + points > max {
                        bail!(
                            "该学生「{type_name}」学分将超过上限 {max}（当前 {total}）",
                            type_name = type_name_of(&self, type_id)?.unwrap_or_default()
                        );
                    }
                }
            }
        }
        let conn = self.conn.lock().unwrap();
        let student_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM students WHERE id = ?1)",
                params![student_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !student_exists {
            bail!("学生不存在（id={student_id}）");
        }
        conn.execute(
            "INSERT INTO credit_records
               (student_id, type_id, points, semester, happened_on, note, operator, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                student_id,
                type_id,
                points,
                semester.trim(),
                happened_on.trim(),
                note.trim(),
                operator.trim(),
                now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn find_credit_type_by_id(&self, id: i64) -> Result<Option<(i64, String, f64)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, max_points FROM credit_types WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    fn summary_by_student_type(&self, student_id: i64, type_id: i64) -> Result<f64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(points), 0) FROM credit_records WHERE student_id = ?1 AND type_id = ?2",
            params![student_id, type_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn update_credit(
        &self,
        id: i64,
        points: Option<f64>,
        type_id: Option<Option<i64>>,
        semester: Option<&str>,
        happened_on: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut sets: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(v) = points {
            if v == 0.0 {
                bail!("分值不能为 0");
            }
            sets.push("points = ?".into());
            values.push(rusqlite::types::Value::Real(v));
        }
        if let Some(v) = type_id {
            sets.push("type_id = ?".into());
            values.push(match v {
                Some(id) => rusqlite::types::Value::Integer(id),
                None => rusqlite::types::Value::Null,
            });
        }
        if let Some(v) = semester {
            sets.push("semester = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = happened_on {
            sets.push("happened_on = ?".into());
            values.push(text_value(v.trim()));
        }
        if let Some(v) = note {
            sets.push("note = ?".into());
            values.push(text_value(v.trim()));
        }
        if sets.is_empty() {
            return Ok(());
        }
        let sql = format!("UPDATE credit_records SET {} WHERE id = ?", sets.join(", "));
        values.push(rusqlite::types::Value::Integer(id));
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        stmt.execute(params.as_slice())?;
        Ok(())
    }

    pub fn delete_credit(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM credit_records WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn find_credit_by_id(&self, id: i64) -> Result<Option<CreditRecordRow>> {
        let conn = self.conn.lock().unwrap();
        query_record_row(&conn, "WHERE r.id = ?1", params![id])
    }

    /// 查询学分记录：可按学生/班级/类型/学期/关键词过滤。
    pub fn query_credits(
        &self,
        student_id: Option<i64>,
        class_id: Option<i64>,
        type_id: Option<i64>,
        semester: &str,
        keyword: &str,
    ) -> Result<Vec<CreditRecordRow>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT r.id, r.student_id, s.student_no, s.name, c.name AS class_name,
                    r.type_id, t.name AS type_name, r.points, r.semester, r.happened_on,
                    r.note, r.operator, r.created_at
             FROM credit_records r
             JOIN students s ON s.id = r.student_id
             LEFT JOIN classes c ON c.id = s.class_id
             LEFT JOIN credit_types t ON t.id = r.type_id",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(v) = student_id {
            conds.push("r.student_id = ?".into());
            values.push(rusqlite::types::Value::Integer(v));
        }
        if let Some(v) = class_id {
            conds.push("s.class_id = ?".into());
            values.push(rusqlite::types::Value::Integer(v));
        }
        if let Some(v) = type_id {
            conds.push("r.type_id = ?".into());
            values.push(rusqlite::types::Value::Integer(v));
        }
        let semester = semester.trim();
        if !semester.is_empty() {
            conds.push("r.semester = ?".into());
            values.push(text_value(semester));
        }
        let keyword = keyword.trim();
        if !keyword.is_empty() {
            conds.push("(s.student_no LIKE ? OR s.name LIKE ? OR r.note LIKE ?)".into());
            let pattern = format!("%{keyword}%");
            values.push(text_value(&pattern));
            values.push(text_value(&pattern));
            values.push(text_value(&pattern));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY r.created_at DESC, r.id DESC");
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(params.as_slice(), record_row_mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 某学生（或某班级全部学生）的学分汇总：按类型 + 总分。
    pub fn summary(&self, student_id: Option<i64>, class_id: Option<i64>) -> Result<CreditSummary> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT COALESCE(t.name, '未分类') AS type_name, COALESCE(SUM(r.points), 0)
             FROM credit_records r
             JOIN students s ON s.id = r.student_id
             LEFT JOIN credit_types t ON t.id = r.type_id",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(v) = student_id {
            conds.push("r.student_id = ?".into());
            values.push(rusqlite::types::Value::Integer(v));
        }
        if let Some(v) = class_id {
            conds.push("s.class_id = ?".into());
            values.push(rusqlite::types::Value::Integer(v));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" GROUP BY t.name ORDER BY type_name");
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let by_type = stmt
            .query_map(params.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let total = by_type.iter().map(|(_, points)| points).sum();
        Ok(CreditSummary { by_type, total })
    }

    // ─────────────────────────── CSV 导入 ───────────────────────────

    /// 批量导入学生：每行「学号,姓名,班级,性别,电话」。
    /// 班级不存在时自动创建。返回 (导入数, 跳过明细)。
    pub fn import_students_csv(&self, csv: &str) -> Result<(usize, Vec<String>)> {
        let mut imported = 0usize;
        let mut skipped = Vec::new();
        for (index, raw_line) in csv.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            let line_no = index + 1;
            let parts: Vec<&str> = line.split(',').map(str::trim).collect();
            if parts.len() < 2 {
                skipped.push(format!("第 {line_no} 行：字段不足（需要 学号,姓名[,班级,性别,电话]）"));
                continue;
            }
            let (student_no, name) = (parts[0], parts[1]);
            let class_name = parts.get(2).map(|v| *v).unwrap_or("");
            let gender = parts.get(3).map(|v| *v).unwrap_or("");
            let phone = parts.get(4).map(|v| *v).unwrap_or("");
            if student_no.is_empty() || name.is_empty() {
                skipped.push(format!("第 {line_no} 行：学号/姓名不能为空"));
                continue;
            }
            let class_id = if class_name.is_empty() {
                None
            } else {
                let existing = self.find_class_by_name(class_name)?;
                match existing {
                    Some((id, _)) => Some(id),
                    None => Some(self.add_class(class_name, "", "", "CSV 导入自动创建")?),
                }
            };
            match self.add_student(student_no, name, class_id, gender, phone, "", "", "", "") {
                Ok(_) => imported += 1,
                Err(err) => skipped.push(format!("第 {line_no} 行（{student_no} {name}）：{err}")),
            }
        }
        Ok((imported, skipped))
    }
}

// ─────────────────────────── 内部工具 ───────────────────────────

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS classes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            grade TEXT DEFAULT '',
            major TEXT DEFAULT '',
            note TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS students (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            student_no TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            class_id INTEGER REFERENCES classes(id) ON DELETE SET NULL,
            gender TEXT DEFAULT '',
            phone TEXT DEFAULT '',
            qq_id TEXT DEFAULT '',
            wecom_id TEXT DEFAULT '',
            feishu_id TEXT DEFAULT '',
            note TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS credit_types (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            description TEXT DEFAULT '',
            max_points REAL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS credit_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            student_id INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
            type_id INTEGER REFERENCES credit_types(id) ON DELETE SET NULL,
            points REAL NOT NULL,
            semester TEXT DEFAULT '',
            happened_on TEXT DEFAULT '',
            note TEXT DEFAULT '',
            operator TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_students_class ON students(class_id);
        CREATE INDEX IF NOT EXISTS idx_students_no ON students(student_no);
        CREATE INDEX IF NOT EXISTS idx_records_student ON credit_records(student_id);
        CREATE INDEX IF NOT EXISTS idx_records_type ON credit_records(type_id);
        CREATE INDEX IF NOT EXISTS idx_records_semester ON credit_records(semester);
        -- APK 问卷申报（导员审批后生效）
        CREATE TABLE IF NOT EXISTS credit_submissions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            student_id INTEGER NOT NULL REFERENCES students(id) ON DELETE CASCADE,
            class_id INTEGER REFERENCES classes(id) ON DELETE SET NULL,
            type_id INTEGER REFERENCES credit_types(id) ON DELETE SET NULL,
            points REAL NOT NULL,
            description TEXT DEFAULT '',
            evidence TEXT DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending',
            device_id TEXT DEFAULT '',
            review_note TEXT DEFAULT '',
            reviewed_at TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_submissions_status ON credit_submissions(status);
        CREATE INDEX IF NOT EXISTS idx_submissions_student ON credit_submissions(student_id);
        CREATE INDEX IF NOT EXISTS idx_submissions_created ON credit_submissions(created_at);
        -- 班级职位（班长/学委等，绑定学生 + 班级）
        CREATE TABLE IF NOT EXISTS roles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            class_id INTEGER NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            student_id INTEGER REFERENCES students(id) ON DELETE SET NULL,
            note TEXT DEFAULT '',
            created_at TEXT NOT NULL,
            UNIQUE(class_id, title)
        );
        -- 辅导员设备（APK 用管理员激活码确认后登记）
        CREATE TABLE IF NOT EXISTS admin_devices (
            device_id TEXT PRIMARY KEY,
            plan TEXT DEFAULT 'admin',
            user_label TEXT DEFAULT '',
            expires_at TEXT DEFAULT '',
            confirmed_at TEXT NOT NULL
        );
        ",
    )?;
    // 迁移：students 增加 APK 设备 ID 列（用于学生自助绑定）
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        let has_apk_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('students') WHERE name = 'apk_id'",
                [],
                |row| row.get::<_, i64>(0),
            )? > 0;
        if !has_apk_id {
            conn.execute_batch(
                "ALTER TABLE students ADD COLUMN apk_id TEXT DEFAULT '';",
            )?;
        }
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

/// 默认学分类型（幂等）。
fn seed_default_types(conn: &Connection) -> Result<()> {
    let defaults: [(&str, &str); 6] = [
        ("思想成长", "党团活动、理论学习、主题班会等"),
        ("志愿公益", "志愿服务、社会实践、公益劳动等"),
        ("文体活动", "文体竞赛、社团活动、文艺演出等"),
        ("学术科研", "学科竞赛、科研训练、论文专利等"),
        ("技能特长", "技能证书、创新创业、职业培训等"),
        ("劳动实践", "劳动教育、实习实训、生活实践等"),
    ];
    for (name, description) in defaults {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credit_types WHERE name = ?1)",
                params![name],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            conn.execute(
                "INSERT INTO credit_types (name, description, max_points) VALUES (?1, ?2, 0)",
                params![name, description],
            )?;
        }
    }
    Ok(())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn student_row_mapper(row: &rusqlite::Row) -> rusqlite::Result<StudentRow> {
    Ok(StudentRow {
        id: row.get(0)?,
        student_no: row.get(1)?,
        name: row.get(2)?,
        class_id: row.get(3)?,
        class_name: row.get(4)?,
        gender: row.get(5)?,
        phone: row.get(6)?,
        qq_id: row.get(7)?,
        wecom_id: row.get(8)?,
        feishu_id: row.get(9)?,
        note: row.get(10)?,
        created_at: row.get(11)?,
    })
}

fn query_student_row(
    conn: &Connection,
    where_clause: &str,
    params: impl rusqlite::Params,
) -> Result<Option<StudentRow>> {
    let sql = format!(
        "SELECT s.id, s.student_no, s.name, s.class_id, c.name AS class_name,
                s.gender, s.phone, s.qq_id, s.wecom_id, s.feishu_id, s.note, s.created_at
         FROM students s LEFT JOIN classes c ON c.id = s.class_id {where_clause}"
    );
    conn.query_row(&sql, params, student_row_mapper)
        .optional()
        .map_err(Into::into)
}

fn record_row_mapper(row: &rusqlite::Row) -> rusqlite::Result<CreditRecordRow> {
    Ok(CreditRecordRow {
        id: row.get(0)?,
        student_id: row.get(1)?,
        student_no: row.get(2)?,
        student_name: row.get(3)?,
        class_name: row.get(4)?,
        type_id: row.get(5)?,
        type_name: row.get(6)?,
        points: row.get(7)?,
        semester: row.get(8)?,
        happened_on: row.get(9)?,
        note: row.get(10)?,
        operator: row.get(11)?,
        created_at: row.get(12)?,
    })
}

// ─────────────────────────── APK 对接（设备绑定 / 职位 / 申报审批） ───────────────────────────

impl CreditsDb {
    // ── APK 设备绑定（学生自助：扫码连接后绑定 学号+姓名） ──

    pub fn find_student_by_apk(&self, apk_id: &str) -> Result<Option<StudentRow>> {
        if apk_id.trim().is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        query_student_row(&conn, "WHERE s.apk_id = ?1", params![apk_id.trim()])
    }

    pub fn bind_student_apk(&self, student_id: i64, apk_id: &str) -> Result<()> {
        let apk_id = apk_id.trim();
        if apk_id.is_empty() {
            bail!("APK 设备 ID 不能为空");
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE students SET apk_id = ?1 WHERE id = ?2",
            params![apk_id, student_id],
        )?;
        Ok(())
    }

    // ── 班级职位（导员创建：班长/学委/团支书…绑定学生） ──

    pub fn role_add(
        &self,
        class_id: i64,
        title: &str,
        student_id: Option<i64>,
        note: &str,
    ) -> Result<i64> {
        let title = title.trim();
        if title.is_empty() {
            bail!("职位名称不能为空");
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO roles (class_id, title, student_id, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![class_id, title, student_id, note.trim(), now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn role_update(
        &self,
        id: i64,
        title: Option<&str>,
        student_id: Option<Option<i64>>,
        note: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut sets: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(v) = title {
            let v = v.trim();
            if v.is_empty() {
                bail!("职位名称不能为空");
            }
            sets.push("title = ?".into());
            values.push(text_value(v));
        }
        if let Some(v) = student_id {
            sets.push("student_id = ?".into());
            values.push(match v {
                Some(id) => rusqlite::types::Value::Integer(id),
                None => rusqlite::types::Value::Null,
            });
        }
        if let Some(v) = note {
            sets.push("note = ?".into());
            values.push(text_value(v.trim()));
        }
        if sets.is_empty() {
            return Ok(());
        }
        values.push(rusqlite::types::Value::Integer(id));
        let sql = format!("UPDATE roles SET {} WHERE id = ?", sets.join(", "));
        conn.execute(&sql, rusqlite::params_from_iter(values))?;
        Ok(())
    }

    pub fn role_delete(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM roles WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn list_roles(&self, class_id: Option<i64>) -> Result<Vec<RoleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT r.id, r.class_id, c.name AS class_name, r.title,
                    r.student_id, s.student_no, s.name AS student_name,
                    r.note, r.created_at
             FROM roles r
             LEFT JOIN classes c ON c.id = r.class_id
             LEFT JOIN students s ON s.id = r.student_id",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(class_id) = class_id {
            conds.push("r.class_id = ?".into());
            values.push(rusqlite::types::Value::Integer(class_id));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY r.id");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), role_row_mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 按学生查职位（判断"职位人员"身份；同学生多职位取第一个）。
    pub fn find_role_by_student(&self, student_id: i64) -> Result<Option<RoleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.class_id, c.name AS class_name, r.title,
                    r.student_id, s.student_no, s.name AS student_name,
                    r.note, r.created_at
             FROM roles r
             LEFT JOIN classes c ON c.id = r.class_id
             LEFT JOIN students s ON s.id = r.student_id
             WHERE r.student_id = ?1
             LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![student_id], role_row_mapper)
            .optional()?;
        Ok(row)
    }

    // ── 辅导员设备（APK 用管理员激活码确认身份） ──

    pub fn is_admin_device(&self, device_id: &str) -> Result<bool> {
        if device_id.trim().is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM admin_devices WHERE device_id = ?1)",
                params![device_id.trim()],
                |row| row.get(0),
            )?;
        if !exists {
            return Ok(false);
        }
        let expires_at: String = conn
            .query_row(
                "SELECT expires_at FROM admin_devices WHERE device_id = ?1",
                params![device_id.trim()],
                |row| row.get(0),
            )?;
        if expires_at.is_empty() {
            // 空 = 永久有效
            return Ok(true);
        }
        match chrono::DateTime::parse_from_rfc3339(&expires_at) {
            Ok(expires) => Ok(expires > chrono::Utc::now()),
            Err(_) => Ok(false),
        }
    }

    pub fn confirm_admin_device(
        &self,
        device_id: &str,
        plan: &str,
        user_label: &str,
        expires_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO admin_devices (device_id, plan, user_label, expires_at, confirmed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![device_id.trim(), plan, user_label, expires_at, now()],
        )?;
        Ok(())
    }

    pub fn remove_admin_device(&self, device_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM admin_devices WHERE device_id = ?1",
            params![device_id.trim()],
        )?;
        Ok(())
    }

    // ── 问卷申报（职位人员提交 → 导员审批 → 生效） ──

    pub fn add_submission(
        &self,
        student_id: i64,
        type_id: Option<i64>,
        points: f64,
        description: &str,
        device_id: &str,
    ) -> Result<i64> {
        if points == 0.0 {
            bail!("申报分值不能为 0");
        }
        let conn = self.conn.lock().unwrap();
        let student = conn
            .query_row(
                "SELECT class_id FROM students WHERE id = ?1",
                params![student_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("学生不存在（id={student_id}）"))?;
        conn.execute(
            "INSERT INTO credit_submissions (student_id, class_id, type_id, points, description, device_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![student_id, student, type_id, points, description.trim(), device_id.trim(), now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_submission_evidence(&self, id: i64, evidence_json: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE credit_submissions SET evidence = ?1 WHERE id = ?2",
            params![evidence_json, id],
        )?;
        Ok(())
    }

    pub fn find_submission(&self, id: i64) -> Result<Option<SubmissionRow>> {
        let conn = self.conn.lock().unwrap();
        query_submission_row(&conn, "WHERE s.id = ?1", params![id])
    }

    pub fn list_submissions(
        &self,
        status: Option<&str>,
        class_id: Option<i64>,
        student_id: Option<i64>,
        date_from: Option<&str>,
        date_to: Option<&str>,
    ) -> Result<Vec<SubmissionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT su.id, su.student_id, st.student_no, st.name AS student_name,
                    su.class_id, c.name AS class_name, su.type_id, t.name AS type_name,
                    su.points, su.description, su.evidence, su.status, su.device_id,
                    su.review_note, su.reviewed_at, su.created_at
             FROM credit_submissions su
             LEFT JOIN students st ON st.id = su.student_id
             LEFT JOIN classes c ON c.id = su.class_id
             LEFT JOIN credit_types t ON t.id = su.type_id",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(status) = status {
            conds.push("su.status = ?".into());
            values.push(text_value(status.trim()));
        }
        if let Some(class_id) = class_id {
            conds.push("su.class_id = ?".into());
            values.push(rusqlite::types::Value::Integer(class_id));
        }
        if let Some(student_id) = student_id {
            conds.push("su.student_id = ?".into());
            values.push(rusqlite::types::Value::Integer(student_id));
        }
        if let Some(from) = date_from {
            conds.push("su.created_at >= ?".into());
            values.push(text_value(from));
        }
        if let Some(to) = date_to {
            conds.push("su.created_at <= ?".into());
            values.push(text_value(to));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY su.id DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), submission_row_mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 导员通过申报：写入 credit_records（含类型上限校验）+ 更新状态。
    pub fn approve_submission(&self, id: i64, review_note: &str, operator: &str) -> Result<()> {
        let submission = self
            .find_submission(id)?
            .ok_or_else(|| anyhow::anyhow!("申报不存在（id={id}）"))?;
        if submission.status == "approved" {
            bail!("该申报已通过，请勿重复审批");
        }
        // 生效为学分记录（含该类型每人上限检查）
        self.add_credit(
            submission.student_id,
            submission.type_id,
            submission.points,
            "",
            "",
            &format!("审批通过：{}", submission.description),
            operator,
        )?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE credit_submissions SET status = 'approved', review_note = ?1, reviewed_at = ?2 WHERE id = ?3",
            params![review_note.trim(), now(), id],
        )?;
        Ok(())
    }

    pub fn reject_submission(&self, id: i64, review_note: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE credit_submissions SET status = 'rejected', review_note = ?1, reviewed_at = ?2 WHERE id = ?3 AND status = 'pending'",
            params![review_note.trim(), now(), id],
        )?;
        if updated == 0 {
            bail!("申报不存在或已审批");
        }
        Ok(())
    }

    /// 按班级/类型聚合申报统计（导员审批面板与 AI 总结用）。
    pub fn submissions_summary(
        &self,
        date_from: Option<&str>,
        date_to: Option<&str>,
    ) -> Result<Vec<SubmissionSummaryRow>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT su.class_id, c.name AS class_name, t.name AS type_name,
                    SUM(CASE WHEN su.status = 'pending' THEN 1 ELSE 0 END) AS pending,
                    SUM(CASE WHEN su.status = 'approved' THEN 1 ELSE 0 END) AS approved,
                    SUM(CASE WHEN su.status = 'rejected' THEN 1 ELSE 0 END) AS rejected,
                    COALESCE(SUM(CASE WHEN su.status = 'approved' THEN su.points ELSE 0 END), 0) AS total_points
             FROM credit_submissions su
             LEFT JOIN classes c ON c.id = su.class_id
             LEFT JOIN credit_types t ON t.id = su.type_id",
        );
        let mut conds: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(from) = date_from {
            conds.push("su.created_at >= ?".into());
            values.push(text_value(from));
        }
        if let Some(to) = date_to {
            conds.push("su.created_at <= ?".into());
            values.push(text_value(to));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(
            " GROUP BY su.class_id, su.type_id
             ORDER BY c.name, t.name",
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok(SubmissionSummaryRow {
                    class_id: row.get(0)?,
                    class_name: row.get(1)?,
                    type_name: row.get(2)?,
                    pending: row.get(3)?,
                    approved: row.get(4)?,
                    rejected: row.get(5)?,
                    total_points: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn role_row_mapper(row: &rusqlite::Row) -> rusqlite::Result<RoleRow> {
    Ok(RoleRow {
        id: row.get(0)?,
        class_id: row.get(1)?,
        class_name: row.get(2)?,
        title: row.get(3)?,
        student_id: row.get(4)?,
        student_no: row.get(5)?,
        student_name: row.get(6)?,
        note: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn submission_row_mapper(row: &rusqlite::Row) -> rusqlite::Result<SubmissionRow> {
    Ok(SubmissionRow {
        id: row.get(0)?,
        student_id: row.get(1)?,
        student_no: row.get(2)?,
        student_name: row.get(3)?,
        class_id: row.get(4)?,
        class_name: row.get(5)?,
        type_id: row.get(6)?,
        type_name: row.get(7)?,
        points: row.get(8)?,
        description: row.get(9)?,
        evidence: row.get(10)?,
        status: row.get(11)?,
        device_id: row.get(12)?,
        review_note: row.get(13)?,
        reviewed_at: row.get(14)?,
        created_at: row.get(15)?,
    })
}

fn query_submission_row(
    conn: &Connection,
    where_clause: &str,
    params: impl rusqlite::Params,
) -> Result<Option<SubmissionRow>> {
    let sql = format!(
        "SELECT su.id, su.student_id, st.student_no, st.name AS student_name,
                su.class_id, c.name AS class_name, su.type_id, t.name AS type_name,
                su.points, su.description, su.evidence, su.status, su.device_id,
                su.review_note, su.reviewed_at, su.created_at
         FROM credit_submissions su
         LEFT JOIN students st ON st.id = su.student_id
         LEFT JOIN classes c ON c.id = su.class_id
         LEFT JOIN credit_types t ON t.id = su.type_id
         {where_clause}",
        where_clause = where_clause,
    );
    let mut stmt = conn.prepare(&sql)?;
    let row = stmt.query_row(params, submission_row_mapper).optional()?;
    Ok(row)
}

fn query_record_row(
    conn: &Connection,
    where_clause: &str,
    params: impl rusqlite::Params,
) -> Result<Option<CreditRecordRow>> {
    let sql = format!(
        "SELECT r.id, r.student_id, s.student_no, s.name, c.name AS class_name,
                r.type_id, t.name AS type_name, r.points, r.semester, r.happened_on,
                r.note, r.operator, r.created_at
         FROM credit_records r
         JOIN students s ON s.id = r.student_id
         LEFT JOIN classes c ON c.id = s.class_id
         LEFT JOIN credit_types t ON t.id = r.type_id {where_clause}"
    );
    conn.query_row(&sql, params, record_row_mapper)
        .optional()
        .map_err(Into::into)
}

fn type_name_of(db: &CreditsDb, type_id: i64) -> Result<Option<String>> {
    Ok(db.find_credit_type_by_id(type_id)?.map(|(_, name, _)| name))
}

fn text_value(value: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(value.to_string())
}

// ─────────────────────────── 测试 ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, CreditsDb) {
        let temp = tempfile::tempdir().unwrap();
        let db = CreditsDb::open_at(&temp.path().join("credit.db")).unwrap();
        (temp, db)
    }

    #[test]
    fn class_and_student_crud() {
        let (_t, db) = temp_db();
        let class_id = db.add_class("计科2301", "2023", "计算机科学与技术", "").unwrap();
        assert_eq!(db.list_classes().unwrap().len(), 1);

        let student_id = db
            .add_student("2023010101", "张三", Some(class_id), "男", "13800000000", "10001", "", "", "")
            .unwrap();
        let found = db.find_student_by_no("2023010101").unwrap().unwrap();
        assert_eq!(found.name, "张三");
        assert_eq!(found.class_name.as_deref(), Some("计科2301"));

        // 重复学号拒绝
        assert!(db
            .add_student("2023010101", "李四", None, "", "", "", "", "", "")
            .is_err());

        // 更新 + 班级删除
        db.update_student(student_id, None, Some("张三丰"), None, None, None, None, None, None, None)
            .unwrap();
        assert_eq!(db.find_student_by_id(student_id).unwrap().unwrap().name, "张三丰");
        let affected = db.delete_class(class_id).unwrap();
        assert_eq!(affected, 1);
        assert!(db.find_student_by_id(student_id).unwrap().unwrap().class_id.is_none());
    }

    #[test]
    fn credit_records_and_summary() {
        let (_t, db) = temp_db();
        let sid = db.add_student("2023010102", "李四", None, "", "", "", "", "", "").unwrap();
        let types = db.list_credit_types().unwrap();
        assert!(!types.is_empty(), "默认学分类型应已播种");
        let (volunteer_id, volunteer_name, _) = db
            .find_credit_type_by_name("志愿公益")
            .unwrap()
            .unwrap();
        assert_eq!(volunteer_name, "志愿公益");

        db.add_credit(sid, Some(volunteer_id), 2.0, "2025-2026-1", "2026-03-10", "敬老院志愿", "辅导员")
            .unwrap();
        db.add_credit(sid, Some(volunteer_id), -0.5, "2025-2026-1", "2026-03-20", "缺勤扣分", "辅导员")
            .unwrap();

        let records = db.query_credits(Some(sid), None, None, "", "").unwrap();
        assert_eq!(records.len(), 2);

        let summary = db.summary(Some(sid), None).unwrap();
        assert_eq!(summary.total, 1.5);
        assert!(summary.by_type.iter().any(|(name, points)| name == "志愿公益" && *points == 1.5));

        // 更新与删除
        let first = records[0].id;
        db.update_credit(first, Some(3.0), None, None, None, None).unwrap();
        assert_eq!(db.find_credit_by_id(first).unwrap().unwrap().points, 3.0);
        db.delete_credit(first).unwrap();
        assert!(db.find_credit_by_id(first).unwrap().is_none());
    }

    #[test]
    fn platform_binding_and_student_delete_cascade() {
        let (_t, db) = temp_db();
        let sid = db
            .add_student("2023010103", "王五", None, "", "", "10002", "zhangsan@wecom", "ou_123", "")
            .unwrap();
        assert_eq!(
            db.find_student_by_platform("qq", "10002").unwrap().unwrap().id,
            sid
        );
        assert_eq!(
            db.find_student_by_platform("wecom", "zhangsan@wecom").unwrap().unwrap().id,
            sid
        );
        assert_eq!(
            db.find_student_by_platform("feishu", "ou_123").unwrap().unwrap().id,
            sid
        );
        assert!(db.find_student_by_platform("qq", "99999").unwrap().is_none());

        let (type_id, _, _) = db.find_credit_type_by_name("文体活动").unwrap().unwrap();
        db.add_credit(sid, Some(type_id), 1.0, "", "", "", "辅导员").unwrap();
        let deleted_records = db.delete_student(sid).unwrap();
        assert_eq!(deleted_records, 1);
        assert!(db.find_student_by_id(sid).unwrap().is_none());
    }

    #[test]
    fn csv_import_creates_classes() {
        let (_t, db) = temp_db();
        let csv = "2023010101,张三,计科2301,男,13800000000\n2023010102,李四,计科2301,女,13900000000\n2023010103,王五,软件2302\n";
        let (imported, skipped) = db.import_students_csv(csv).unwrap();
        assert_eq!(imported, 3);
        assert!(skipped.is_empty());
        assert_eq!(db.list_classes().unwrap().len(), 2);
        assert_eq!(db.query_students(None, "李四").unwrap().len(), 1);
    }

    #[test]
    fn csv_import_reports_bad_lines() {
        let (_t, db) = temp_db();
        let csv = "2023010101,张三\n,空学号\n2023010101,重复\n赵六\n";
        let (imported, skipped) = db.import_students_csv(csv).unwrap();
        assert_eq!(imported, 1);
        assert_eq!(skipped.len(), 3);
    }
}
