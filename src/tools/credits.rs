//! 学分管理工具（希尔娅的主业）。
//!
//! 服务对象：大学辅导员（管理员）与学生。
//! - 管理员（辅导员）：班级/学生/学分类型/学分记录的增删改查 + CSV 导入
//! - 学生：只能查询自己的学分与个人信息（按平台绑定识别身份）
//!
//! 权限判定：
//! - 本地（CLI/面板）→ 管理员
//! - 桥接消息 → bridges.json 的 admins 映射（平台 → 辅导员 ID 列表）
//!   非管理员 → 按平台绑定找学生，未绑定则引导先 student_bind（学号+姓名）

use super::{ToolRegistry, ToolSpec};
use crate::i18n::agent_text as t;
use crate::paths::GqyPaths;
use crate::state::{CreditRecordRow, CreditsDb, StudentRow};
use anyhow::{bail, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
enum Role {
    Admin,
    /// 班级职位人员（班长/学委等）：可填写学分申报
    Officer { student: StudentRow, title: String },
    Student(Option<StudentRow>),
}

fn open_db(paths: &GqyPaths) -> Result<CreditsDb> {
    CreditsDb::open(&paths.data_dir)
}

fn current_role(paths: &GqyPaths) -> Role {
    if !crate::bridges::is_bridged() {
        return Role::Admin;
    }
    let identity = crate::bridges::current_identity();
    let bridges = crate::bridges::load(paths).unwrap_or_default();
    if crate::bridges::is_admin(&bridges, &identity.platform, &identity.user_id) {
        return Role::Admin;
    }
    let db = match open_db(paths) {
        Ok(db) => db,
        Err(_) => return Role::Student(None),
    };
    // APK 辅导员设备：用管理员激活码确认过身份
    if identity.platform == "apk" && db.is_admin_device(&identity.user_id).unwrap_or(false) {
        return Role::Admin;
    }
    let student = match db.find_student_by_platform(&identity.platform, &identity.user_id) {
        Ok(Some(student)) => student,
        _ => return Role::Student(None),
    };
    // 班级职位人员（班长/学委/团支书…）→ 可填写学分申报
    if let Ok(Some(role)) = db.find_role_by_student(student.id) {
        return Role::Officer {
            student,
            title: role.title,
        };
    }
    Role::Student(Some(student))
}

fn require_admin(role: &Role) -> Result<()> {
    match role {
        Role::Admin => Ok(()),
        _ => bail!("该操作需要管理员（辅导员）权限；学生只能查询自己的信息"),
    }
}

fn require_officer(role: &Role) -> Result<()> {
    match role {
        Role::Admin | Role::Officer { .. } => Ok(()),
        _ => bail!("只有班级职位人员（如班长）才能填写学分申报"),
    }
}

fn ok(text: String) -> String {
    json!({ "ok": true, "text": text }).to_string()
}

fn fail(message: &str) -> String {
    json!({ "ok": false, "error": message }).to_string()
}

// ─────────────────────────── 解析辅助 ───────────────────────────

fn student_from_args(db: &CreditsDb, args: &Value) -> Result<StudentRow> {
    if let Some(id) = args.get("student_id").and_then(Value::as_i64) {
        return db
            .find_student_by_id(id)?
            .ok_or_else(|| anyhow::anyhow!("学生不存在（id={id}）"));
    }
    if let Some(no) = args.get("student_no").and_then(Value::as_str) {
        return db
            .find_student_by_no(no)?
            .ok_or_else(|| anyhow::anyhow!("学号 {no} 不存在（可先 student_add 建档）"));
    }
    bail!("需要提供 student_id 或 student_no");
}

fn class_id_from_args(db: &CreditsDb, args: &Value) -> Result<Option<i64>> {
    if let Some(id) = args.get("class_id").and_then(Value::as_i64) {
        return Ok(Some(id));
    }
    if let Some(name) = args.get("class_name").and_then(Value::as_str) {
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        let (id, _) = db
            .find_class_by_name(name)?
            .ok_or_else(|| anyhow::anyhow!("班级「{name}」不存在（可先 class_add 创建）"))?;
        return Ok(Some(id));
    }
    Ok(None)
}

fn type_id_from_args(db: &CreditsDb, args: &Value) -> Result<Option<i64>> {
    if let Some(name) = args.get("type_name").and_then(Value::as_str) {
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        let (id, _, _) = db
            .find_credit_type_by_name(name)?
            .ok_or_else(|| anyhow::anyhow!("学分类型「{name}」不存在（可先 credit_type_add 添加）"))?;
        return Ok(Some(id));
    }
    if let Some(id) = args.get("type_id").and_then(Value::as_i64) {
        return Ok(Some(id));
    }
    Ok(None)
}

fn render_records(records: &[CreditRecordRow]) -> String {
    if records.is_empty() {
        return "没有找到学分记录。".to_string();
    }
    let mut lines = Vec::new();
    for record in records {
        let type_name = record.type_name.as_deref().unwrap_or("未分类");
        let sign = if record.points >= 0.0 { "+" } else { "" };
        lines.push(format!(
            "#{} {}（{}）{} {}分｜{}｜{}｜{}",
            record.id,
            record.student_name,
            record.student_no,
            type_name,
            format!("{sign}{}", record.points),
            record.semester,
            record.happened_on,
            record.note
        ));
    }
    lines.join("\n")
}

fn render_summary(summary: &crate::state::CreditSummary) -> String {
    let mut lines = Vec::new();
    for (name, points) in &summary.by_type {
        lines.push(format!("  {}：{} 分", name, points));
    }
    lines.push(format!("  合计：{} 分", summary.total));
    lines.join("\n")
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

// ─────────────────────────── 学分记录 ───────────────────────────

async fn credit_add(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let student = match student_from_args(&db, &args) {
        Ok(s) => s,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let points = match args.get("points").and_then(Value::as_f64) {
        Some(p) if p != 0.0 => p,
        _ => return Ok(fail("points 必须是非零数值（加分正数、扣分负数）")),
    };
    let type_id = match type_id_from_args(&db, &args) {
        Ok(v) => v,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let operator = current_operator();
    match db.add_credit(
        student.id,
        type_id,
        points,
        arg_str(&args, "semester").unwrap_or(""),
        arg_str(&args, "happened_on").unwrap_or(""),
        arg_str(&args, "note").unwrap_or(""),
        &operator,
    ) {
        Ok(record_id) => {
            let sign = if points >= 0.0 { "+" } else { "" };
            let summary = db.summary(Some(student.id), None)?;
            let mut text = format!(
                "已记录：{}（{}）{}{} 分（记录 #{}）\n当前总学分 {}",
                student.name, student.student_no, sign, points, record_id, summary.total
            );
            if let Some(type_name) = args.get("type_name").and_then(Value::as_str) {
                text.push_str(&format!("，其中{} {} 分", type_name, summary_total_for(&summary, type_name)));
            }
            Ok(ok(text))
        }
        Err(err) => Ok(fail(&err.to_string())),
    }
}

fn summary_total_for(summary: &crate::state::CreditSummary, type_name: &str) -> f64 {
    summary
        .by_type
        .iter()
        .find(|(name, _)| name == type_name)
        .map(|(_, points)| *points)
        .unwrap_or(0.0)
}

fn current_operator() -> String {
    if crate::bridges::is_bridged() {
        let identity = crate::bridges::current_identity();
        format!("{}:{}", identity.platform, identity.user_id)
    } else {
        "本地".to_string()
    }
}

async fn credit_update(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let record_id = match args.get("record_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要 record_id")),
    };
    let existing = match db.find_credit_by_id(record_id)? {
        Some(record) => record,
        None => return Ok(fail(&format!("记录 #{} 不存在", record_id))),
    };
    let type_id = match type_id_from_args(&db, &args) {
        Ok(v) => v,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let points = args.get("points").and_then(Value::as_f64).filter(|p| *p != 0.0);
    match db.update_credit(
        record_id,
        points,
        Some(type_id),
        arg_str(&args, "semester"),
        arg_str(&args, "happened_on"),
        arg_str(&args, "note"),
    ) {
        Ok(()) => {
            let updated = db.find_credit_by_id(record_id)?.unwrap_or(existing);
            let sign = if updated.points >= 0.0 { "+" } else { "" };
            Ok(ok(format!(
                "已更新记录 #{}：{}（{}）{} {} 分",
                record_id,
                updated.student_name,
                updated.student_no,
                updated.type_name.as_deref().unwrap_or("未分类"),
                format!("{sign}{}", updated.points)
            )))
        }
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn credit_delete(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let record_id = match args.get("record_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要 record_id")),
    };
    let existing = match db.find_credit_by_id(record_id)? {
        Some(record) => record,
        None => return Ok(fail(&format!("记录 #{} 不存在", record_id))),
    };
    db.delete_credit(record_id)?;
    Ok(ok(format!(
        "已删除记录 #{}：{}（{}）{} {} 分",
        record_id,
        existing.student_name,
        existing.student_no,
        existing.type_name.as_deref().unwrap_or("未分类"),
        existing.points
    )))
}

async fn credit_query(args: Value, paths: GqyPaths) -> Result<String> {
    let db = open_db(&paths)?;
    let role = current_role(&paths);
    match role {
        Role::Admin => {
            let student = match student_from_args(&db, &args) {
                Ok(s) => Some(s),
                Err(_) => None,
            };
            let class_id = match class_id_from_args(&db, &args) {
                Ok(v) => v,
                Err(err) => return Ok(fail(&err.to_string())),
            };
            let type_id = match type_id_from_args(&db, &args) {
                Ok(v) => v,
                Err(err) => return Ok(fail(&err.to_string())),
            };
            let records = db.query_credits(
                student.as_ref().map(|s| s.id),
                class_id,
                type_id,
                arg_str(&args, "semester").unwrap_or(""),
                arg_str(&args, "keyword").unwrap_or(""),
            )?;
            let mut text = String::new();
            if let Some(student) = &student {
                let summary = db.summary(Some(student.id), None)?;
                text.push_str(&format!(
                    "{}（{}）学分汇总：\n{}\n",
                    student.name,
                    student.student_no,
                    render_summary(&summary)
                ));
            } else if let Some(class_id) = class_id {
                let summary = db.summary(None, Some(class_id))?;
                text.push_str(&format!("班级学分汇总：\n{}\n", render_summary(&summary)));
            }
            text.push_str(&render_records(&records));
            Ok(ok(text))
        }
        Role::Student(Some(student)) | Role::Officer { student, .. } => {
            let records = db.query_credits(Some(student.id), None, None, "", "")?;
            let summary = db.summary(Some(student.id), None)?;
            let mut text = format!(
                "{}（{}）你的学分：\n{}\n",
                student.name,
                student.student_no,
                render_summary(&summary)
            );
            text.push_str(&render_records(&records));
            Ok(ok(text))
        }
        Role::Student(None) => Ok(ok(
            "你的账号还没有绑定学号。请把学号和姓名发给我（例如：绑定 2023010101 张三），我会帮你完成绑定。".to_string(),
        )),
    }
}

// ─────────────────────────── 学生 ───────────────────────────

async fn student_add(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let class_id = match class_id_from_args(&db, &args) {
        Ok(v) => v,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    match db.add_student(
        arg_str(&args, "student_no").unwrap_or(""),
        arg_str(&args, "name").unwrap_or(""),
        class_id,
        arg_str(&args, "gender").unwrap_or(""),
        arg_str(&args, "phone").unwrap_or(""),
        arg_str(&args, "qq_id").unwrap_or(""),
        arg_str(&args, "wecom_id").unwrap_or(""),
        arg_str(&args, "feishu_id").unwrap_or(""),
        arg_str(&args, "note").unwrap_or(""),
    ) {
        Ok(id) => Ok(ok(format!("已建档：{}（{}）学生 #{}", arg_str(&args, "name").unwrap_or(""), arg_str(&args, "student_no").unwrap_or(""), id))),
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn student_update(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let student = match student_from_args(&db, &args) {
        Ok(s) => s,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let class_id = match class_id_from_args(&db, &args) {
        Ok(v) => v,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    db.update_student(
        student.id,
        arg_str(&args, "student_no"),
        arg_str(&args, "name"),
        Some(class_id),
        arg_str(&args, "gender"),
        arg_str(&args, "phone"),
        arg_str(&args, "qq_id"),
        arg_str(&args, "wecom_id"),
        arg_str(&args, "feishu_id"),
        arg_str(&args, "note"),
    )?;
    Ok(ok(format!("已更新学生 #{}（{}）", student.id, student.name)))
}

async fn student_delete(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let student = match student_from_args(&db, &args) {
        Ok(s) => s,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let deleted_records = db.delete_student(student.id)?;
    Ok(ok(format!(
        "已删除学生 {}（{}），同时删除其 {} 条学分记录",
        student.name, student.student_no, deleted_records
    )))
}

async fn student_query(args: Value, paths: GqyPaths) -> Result<String> {
    let db = open_db(&paths)?;
    let role = current_role(&paths);
    match role {
        Role::Admin => {
            let class_id = match class_id_from_args(&db, &args) {
                Ok(v) => v,
                Err(err) => return Ok(fail(&err.to_string())),
            };
            let students = db.query_students(class_id, arg_str(&args, "keyword").unwrap_or(""))?;
            if students.is_empty() {
                return Ok(ok("没有找到匹配的学生。".to_string()));
            }
            let mut lines = Vec::new();
            for student in &students {
                let summary = db.summary(Some(student.id), None)?;
                lines.push(format!(
                    "{} {} ｜ {} ｜ 学分 {}",
                    student.student_no,
                    student.name,
                    student.class_name.as_deref().unwrap_or("未分班"),
                    summary.total
                ));
            }
            Ok(ok(lines.join("\n")))
        }
        Role::Student(Some(student)) | Role::Officer { student, .. } => {
            let summary = db.summary(Some(student.id), None)?;
            Ok(ok(format!(
                "{}（{}）｜{}｜电话 {}｜总学分 {}",
                student.name,
                student.student_no,
                student.class_name.as_deref().unwrap_or("未分班"),
                if student.phone.is_empty() { "未登记" } else { &student.phone },
                summary.total
            )))
        }
        Role::Student(None) => Ok(ok(
            "你的账号还没有绑定学号。请把学号和姓名发给我（例如：绑定 2023010101 张三），我会帮你完成绑定。".to_string(),
        )),
    }
}

/// 学生自助绑定：学号 + 姓名匹配后，把当前平台 ID 绑定到该学生。
async fn student_bind(args: Value, paths: GqyPaths) -> Result<String> {
    let identity = crate::bridges::current_identity();
    if identity.platform.is_empty() || identity.user_id.is_empty() {
        return Ok(ok("当前是本机使用，无需绑定（本机即为管理员）。".to_string()));
    }
    let db = open_db(&paths)?;
    let student_no = match arg_str(&args, "student_no") {
        Some(v) => v.to_string(),
        None => return Ok(fail("需要提供学号（student_no）")),
    };
    let name = match arg_str(&args, "name") {
        Some(v) => v.to_string(),
        None => return Ok(fail("需要提供姓名（name）")),
    };
    let student = match db.find_student_by_no(&student_no)? {
        Some(student) => student,
        None => return Ok(fail(&format!("学号 {student_no} 不存在，请联系辅导员建档"))),
    };
    if student.name != name {
        return Ok(fail(&format!("姓名与学号不匹配（{} ≠ {}），请核对后重试", student.name, name)));
    }
    if identity.platform == "apk" {
        // APK 设备绑定（存 apk_id 列）
        if let Some(existing) = db.find_student_by_apk(&identity.user_id)? {
            if existing.id != student.id {
                return Ok(fail("该 APK 设备已绑定其他学号，请联系辅导员处理"));
            }
        }
        db.bind_student_apk(student.id, &identity.user_id)?;
        return Ok(ok(format!(
            "绑定成功：{}（{}）已绑定当前 App。以后可以直接发消息查学分。",
            student.name, student.student_no
        )));
    }
    let (current, qq, wecom, feishu) = match identity.platform.as_str() {
        "qq" => (student.qq_id.clone(), Some(identity.user_id.as_str()), None, None),
        "wecom" => (student.wecom_id.clone(), None, Some(identity.user_id.as_str()), None),
        "feishu" => (student.feishu_id.clone(), None, None, Some(identity.user_id.as_str())),
        _ => return Ok(fail(&format!("不支持的平台：{}", identity.platform))),
    };
    if !current.is_empty() && current != identity.user_id {
        return Ok(fail(&format!("该学号已被其他账号绑定，请联系辅导员处理")));
    }
    db.update_student(student.id, None, None, None, None, None, qq, wecom, feishu, None)?;
    Ok(ok(format!(
        "绑定成功：{}（{}）已绑定当前 {} 账号。以后可以直接发消息查学分。",
        student.name, student.student_no, identity.platform
    )))
}

// ─────────────────────────── 班级 ───────────────────────────

async fn class_add(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    match db.add_class(
        arg_str(&args, "name").unwrap_or(""),
        arg_str(&args, "grade").unwrap_or(""),
        arg_str(&args, "major").unwrap_or(""),
        arg_str(&args, "note").unwrap_or(""),
    ) {
        Ok(id) => Ok(ok(format!("已创建班级：{}（#{}）", arg_str(&args, "name").unwrap_or(""), id))),
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn class_update(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let class_id = match args.get("class_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要 class_id")),
    };
    db.update_class(
        class_id,
        arg_str(&args, "name"),
        arg_str(&args, "grade"),
        arg_str(&args, "major"),
        arg_str(&args, "note"),
    )?;
    Ok(ok(format!("已更新班级 #{}", class_id)))
}

async fn class_delete(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let class_id = match args.get("class_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要 class_id")),
    };
    let affected = db.delete_class(class_id)?;
    Ok(ok(format!(
        "已删除班级 #{}（{} 名学生已解除班级关联，学生与学分记录保留）",
        class_id, affected
    )))
}

async fn class_query(_args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let classes = db.list_classes()?;
    if classes.is_empty() {
        return Ok(ok("还没有班级，用 class_add 创建。".to_string()));
    }
    let lines: Vec<String> = classes
        .iter()
        .map(|c| format!("#{} {} ｜ {} ｜ {} 人", c.id, c.name, c.grade, c.student_count))
        .collect();
    Ok(ok(lines.join("\n")))
}

// ─────────────────────────── 学分类型 / 导入 ───────────────────────────

async fn credit_type_add(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let max_points = args.get("max_points").and_then(Value::as_f64).unwrap_or(0.0);
    match db.add_credit_type(
        arg_str(&args, "name").unwrap_or(""),
        arg_str(&args, "description").unwrap_or(""),
        max_points,
    ) {
        Ok(id) => Ok(ok(format!("已添加学分类型：{}（#{}）", arg_str(&args, "name").unwrap_or(""), id))),
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn credit_type_list(_args: Value, paths: GqyPaths) -> Result<String> {
    let db = open_db(&paths)?;
    let types = db.list_credit_types()?;
    if types.is_empty() {
        return Ok(ok("还没有学分类型。".to_string()));
    }
    let lines: Vec<String> = types
        .iter()
        .map(|t| {
            let limit = if t.max_points > 0.0 {
                format!("（上限 {} 分）", t.max_points)
            } else {
                String::new()
            };
            format!("{}：{}{}", t.name, t.description, limit)
        })
        .collect();
    Ok(ok(lines.join("\n")))
}

async fn credits_import(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let csv = match arg_str(&args, "csv") {
        Some(v) => v.to_string(),
        None => return Ok(fail("需要 csv 内容（每行：学号,姓名,班级,性别,电话）")),
    };
    match db.import_students_csv(&csv) {
        Ok((imported, skipped)) => {
            let mut text = format!("导入完成：成功 {} 人", imported);
            if !skipped.is_empty() {
                text.push_str(&format!("，跳过 {} 条：\n{}", skipped.len(), skipped.join("\n")));
            }
            Ok(ok(text))
        }
        Err(err) => Ok(fail(&err.to_string())),
    }
}

// ─────────────────────────── 注册 ───────────────────────────

pub fn register(registry: &mut ToolRegistry, paths: GqyPaths) {
    let paths_for_tool_1 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_add",
        t(
            "Add or deduct a credit record for a student (admin only). Points are positive for award, negative for deduction. Resolve the student by student_no or student_id, the credit type by type_name. Summarizes the student's new total.",
            "给学生加/扣学分记录（仅辅导员可用）。points 正数为加分、负数为扣分。按学号/学生ID定位学生，按类型名定位学分类型。返回学生最新学分汇总。",
        ),
        json!({
            "type": "object",
            "properties": {
                "student_no": {"type": "string", "description": t("Student number.", "学号。")},
                "student_id": {"type": "integer", "description": t("Student database id (alternative to student_no).", "学生数据库 ID（与学号二选一）。")},
                "points": {"type": "number", "description": t("Points to add (positive) or deduct (negative). Must be non-zero.", "分值：加分为正数，扣分为负数。不能为 0。")},
                "type_name": {"type": "string", "description": t("Credit type name (e.g. 志愿公益).", "学分类型名称（如 志愿公益）。")},
                "semester": {"type": "string", "description": t("Semester, e.g. 2025-2026-1.", "学期，如 2025-2026-1。")},
                "happened_on": {"type": "string", "description": t("Date of the activity, e.g. 2026-03-10.", "发生日期，如 2026-03-10。")},
                "note": {"type": "string", "description": t("Remark.", "备注。")}
            },
            "required": ["points"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_1.clone();
            async move { credit_add(args, paths).await }
        },
    ));

    let paths_for_tool_2 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_update",
        t(
            "Modify an existing credit record (admin only). Locate it by record_id, change points / type_name / semester / happened_on / note.",
            "修改已有学分记录（仅辅导员可用）。按 record_id 定位，可改分值/类型/学期/日期/备注。",
        ),
        json!({
            "type": "object",
            "properties": {
                "record_id": {"type": "integer", "description": t("Credit record id.", "学分记录 ID。")},
                "points": {"type": "number", "description": t("New points (non-zero).", "新分值（非零）。")},
                "type_name": {"type": "string", "description": t("New credit type name.", "新的学分类型名称。")},
                "semester": {"type": "string", "description": t("New semester.", "新学期。")},
                "happened_on": {"type": "string", "description": t("New date.", "新日期。")},
                "note": {"type": "string", "description": t("New remark.", "新备注。")}
            },
            "required": ["record_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_2.clone();
            async move { credit_update(args, paths).await }
        },
    ));

    let paths_for_tool_3 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_delete",
        t(
            "Delete a credit record permanently (admin only). Confirm the record content with the user before deleting.",
            "永久删除一条学分记录（仅辅导员可用）。删除前先向辅导员复述要删的记录内容确认。",
        ),
        json!({
            "type": "object",
            "properties": {
                "record_id": {"type": "integer", "description": t("Credit record id.", "学分记录 ID。")}
            },
            "required": ["record_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_3.clone();
            async move { credit_delete(args, paths).await }
        },
    ));

    let paths_for_tool_4 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_query",
        t(
            "Query credit records and summaries. Admins can filter by student / class / type / semester / keyword; students can only see their own records.",
            "查询学分记录与汇总。辅导员可按学生/班级/类型/学期/关键词筛选；学生只能查自己的记录。",
        ),
        json!({
            "type": "object",
            "properties": {
                "student_no": {"type": "string", "description": t("Filter by student number.", "按学号筛选。")},
                "student_id": {"type": "integer", "description": t("Filter by student id.", "按学生 ID 筛选。")},
                "class_id": {"type": "integer", "description": t("Filter by class id.", "按班级 ID 筛选。")},
                "class_name": {"type": "string", "description": t("Filter by class name.", "按班级名筛选。")},
                "type_name": {"type": "string", "description": t("Filter by credit type.", "按学分类型筛选。")},
                "semester": {"type": "string", "description": t("Filter by semester.", "按学期筛选。")},
                "keyword": {"type": "string", "description": t("Keyword in student no / name / note.", "学号/姓名/备注关键词。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_4.clone();
            async move { credit_query(args, paths).await }
        },
    ));

    let paths_for_tool_5 = paths.clone();
    registry.register(ToolSpec::new(
        "student_add",
        t(
            "Create a student record (admin only). Required: student_no, name. Optional: class_name/class_id, gender, phone, platform bindings.",
            "创建学生档案（仅辅导员可用）。必填：学号、姓名；可选：班级、性别、电话、平台绑定。",
        ),
        json!({
            "type": "object",
            "properties": {
                "student_no": {"type": "string", "description": t("Student number.", "学号。")},
                "name": {"type": "string", "description": t("Student name.", "姓名。")},
                "class_name": {"type": "string", "description": t("Class name (created if missing).", "班级名（不存在时自动创建）。")},
                "class_id": {"type": "integer", "description": t("Class id (alternative to class_name).", "班级 ID（与 class_name 二选一）。")},
                "gender": {"type": "string", "description": t("Gender.", "性别。")},
                "phone": {"type": "string", "description": t("Phone number.", "电话。")},
                "qq_id": {"type": "string", "description": t("QQ binding.", "QQ 绑定。")},
                "wecom_id": {"type": "string", "description": t("WeCom userid binding.", "企业微信 userid 绑定。")},
                "feishu_id": {"type": "string", "description": t("Feishu open_id binding.", "飞书 open_id 绑定。")},
                "note": {"type": "string", "description": t("Remark.", "备注。")}
            },
            "required": ["student_no", "name"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_5.clone();
            async move { student_add(args, paths).await }
        },
    ));

    let paths_for_tool_6 = paths.clone();
    registry.register(ToolSpec::new(
        "student_update",
        t(
            "Update a student record (admin only). Locate by student_id or student_no; only provided fields are changed.",
            "修改学生档案（仅辅导员可用）。按 student_id 或 student_no 定位，只改提供的字段。",
        ),
        json!({
            "type": "object",
            "properties": {
                "student_id": {"type": "integer", "description": t("Student id.", "学生 ID。")},
                "student_no": {"type": "string", "description": t("Student number (alternative).", "学号（二选一）。")},
                "name": {"type": "string", "description": t("New name.", "新姓名。")},
                "class_name": {"type": "string", "description": t("New class name.", "新班级名。")},
                "class_id": {"type": "integer", "description": t("New class id.", "新班级 ID。")},
                "gender": {"type": "string", "description": t("New gender.", "新性别。")},
                "phone": {"type": "string", "description": t("New phone.", "新电话。")},
                "qq_id": {"type": "string", "description": t("New QQ binding.", "新 QQ 绑定。")},
                "wecom_id": {"type": "string", "description": t("New WeCom binding.", "新企业微信绑定。")},
                "feishu_id": {"type": "string", "description": t("New Feishu binding.", "新飞书绑定。")},
                "note": {"type": "string", "description": t("New remark.", "新备注。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_6.clone();
            async move { student_update(args, paths).await }
        },
    ));

    let paths_for_tool_7 = paths.clone();
    registry.register(ToolSpec::new(
        "student_delete",
        t(
            "Delete a student and all their credit records (admin only). Confirm with the counselor first.",
            "删除学生及其全部学分记录（仅辅导员可用，不可逆）。执行前先向辅导员确认。",
        ),
        json!({
            "type": "object",
            "properties": {
                "student_id": {"type": "integer", "description": t("Student id.", "学生 ID。")},
                "student_no": {"type": "string", "description": t("Student number (alternative).", "学号（二选一）。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_7.clone();
            async move { student_delete(args, paths).await }
        },
    ));

    let paths_for_tool_8 = paths.clone();
    registry.register(ToolSpec::new(
        "student_query",
        t(
            "Query students. Admins filter by class/keyword and get totals; students get their own profile.",
            "查询学生。辅导员按班级/关键词筛选并看学分汇总；学生查自己的信息。",
        ),
        json!({
            "type": "object",
            "properties": {
                "class_id": {"type": "integer", "description": t("Filter by class id.", "按班级 ID 筛选。")},
                "class_name": {"type": "string", "description": t("Filter by class name.", "按班级名筛选。")},
                "keyword": {"type": "string", "description": t("Keyword in student no / name.", "学号/姓名关键词。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_8.clone();
            async move { student_query(args, paths).await }
        },
    ));

    let paths_for_tool_9 = paths.clone();
    registry.register(ToolSpec::new(
        "student_bind",
        t(
            "Self-service binding for students: bind the current chat platform account to a student by student_no + name. Ask the student for their student number and name when unbound.",
            "学生自助绑定：用学号+姓名把当前平台账号绑定到学生档案。未绑定时引导用户提供学号和姓名。",
        ),
        json!({
            "type": "object",
            "properties": {
                "student_no": {"type": "string", "description": t("Student number.", "学号。")},
                "name": {"type": "string", "description": t("Student name for verification.", "用于校验的姓名。")}
            },
            "required": ["student_no", "name"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_9.clone();
            async move { student_bind(args, paths).await }
        },
    ));

    let paths_for_tool_10 = paths.clone();
    registry.register(ToolSpec::new(
        "class_add",
        t(
            "Create a class (admin only). Name is required; grade / major / note optional.",
            "创建班级（仅辅导员可用）。名称必填，年级/专业/备注可选。",
        ),
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": t("Class name, e.g. 计科2301.", "班级名称，如 计科2301。")},
                "grade": {"type": "string", "description": t("Grade, e.g. 2023.", "年级，如 2023。")},
                "major": {"type": "string", "description": t("Major.", "专业。")},
                "note": {"type": "string", "description": t("Remark.", "备注。")}
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_10.clone();
            async move { class_add(args, paths).await }
        },
    ));

    let paths_for_tool_11 = paths.clone();
    registry.register(ToolSpec::new(
        "class_update",
        t(
            "Update a class (admin only). Only provided fields are changed.",
            "修改班级（仅辅导员可用）。只改提供的字段。",
        ),
        json!({
            "type": "object",
            "properties": {
                "class_id": {"type": "integer", "description": t("Class id.", "班级 ID。")},
                "name": {"type": "string", "description": t("New class name.", "新班级名。")},
                "grade": {"type": "string", "description": t("New grade.", "新年级。")},
                "major": {"type": "string", "description": t("New major.", "新专业。")},
                "note": {"type": "string", "description": t("New remark.", "新备注。")}
            },
            "required": ["class_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_11.clone();
            async move { class_update(args, paths).await }
        },
    ));

    let paths_for_tool_12 = paths.clone();
    registry.register(ToolSpec::new(
        "class_delete",
        t(
            "Delete a class (admin only). Students keep their records but lose the class association.",
            "删除班级（仅辅导员可用）。学生保留但解除班级关联。",
        ),
        json!({
            "type": "object",
            "properties": {
                "class_id": {"type": "integer", "description": t("Class id.", "班级 ID。")}
            },
            "required": ["class_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_12.clone();
            async move { class_delete(args, paths).await }
        },
    ));

    let paths_for_tool_13 = paths.clone();
    registry.register(ToolSpec::new(
        "class_query",
        t(
            "List all classes with student counts (admin only).",
            "列出全部班级与学生人数（仅辅导员可用）。",
        ),
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_13.clone();
            async move { class_query(args, paths).await }
        },
    ));

    let paths_for_tool_14 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_type_add",
        t(
            "Add a credit type (admin only), e.g. 志愿服务 / 学术科研. max_points optionally caps the per-student total.",
            "添加学分类型（仅辅导员可用），如 志愿服务/学术科研。max_points 可设该类型每人上限。",
        ),
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": t("Type name.", "类型名称。")},
                "description": {"type": "string", "description": t("Description.", "说明。")},
                "max_points": {"type": "number", "description": t("Optional per-student cap (0 = unlimited).", "每人上限（0 = 不限）。")}
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_14.clone();
            async move { credit_type_add(args, paths).await }
        },
    ));

    let paths_for_tool_15 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_type_list",
        t(
            "List all credit types.",
            "列出全部学分类型。",
        ),
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_15.clone();
            async move { credit_type_list(args, paths).await }
        },
    ));

    let paths_for_tool_16 = paths.clone();
    registry.register(ToolSpec::new(
        "credits_import",
        t(
            "Batch import students from CSV (admin only). Each line: 学号,姓名,班级,性别,电话. Classes are auto-created when missing.",
            "CSV 批量导入学生（仅辅导员可用）。每行：学号,姓名,班级,性别,电话。班级不存在时自动创建。",
        ),
        json!({
            "type": "object",
            "properties": {
                "csv": {"type": "string", "description": t("CSV content, one student per line.", "CSV 内容，每行一个学生。")}
            },
            "required": ["csv"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_16.clone();
            async move { credits_import(args, paths).await }
        },
    ));

    // ─────────── APK 对接扩展：班级职位 / 问卷申报 / 审批 ───────────

    let paths_for_tool_17 = paths.clone();
    registry.register(ToolSpec::new(
        "role_add",
        t(
            "Add a class officer role (admin only), e.g. 班长/学委/团支书. Binds a student to a class + title. Officers can submit credit applications from the App.",
            "新增班级职位（仅辅导员可用），如 班长/学委/团支书。将学生绑定到班级与职位。职位人员可在 App 上填写学分申报。",
        ),
        json!({
            "type": "object",
            "properties": {
                "class_name": {"type": "string", "description": t("Class name.", "班级名称。")},
                "title": {"type": "string", "description": t("Role title, e.g. 班长.", "职位名称，如 班长。")},
                "student_no": {"type": "string", "description": t("Student number to bind.", "要绑定的学号。")},
                "note": {"type": "string", "description": t("Optional note.", "备注（可选）。")}
            },
            "required": ["class_name", "title"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_17.clone();
            async move { role_add(args, paths).await }
        },
    ));

    let paths_for_tool_18 = paths.clone();
    registry.register(ToolSpec::new(
        "role_update",
        t(
            "Update a class officer role (admin only): change title or rebind a different student.",
            "修改班级职位（仅辅导员可用）：改职位名称或换绑学生。",
        ),
        json!({
            "type": "object",
            "properties": {
                "role_id": {"type": "integer", "description": t("Role id.", "职位 ID。")},
                "title": {"type": "string", "description": t("New role title.", "新职位名称。")},
                "student_no": {"type": "string", "description": t("New student number, or empty to unbind.", "新学号（留空=解除绑定）。")},
                "note": {"type": "string", "description": t("Optional note.", "备注（可选）。")}
            },
            "required": ["role_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_18.clone();
            async move { role_update(args, paths).await }
        },
    ));

    let paths_for_tool_19 = paths.clone();
    registry.register(ToolSpec::new(
        "role_delete",
        t(
            "Delete a class officer role (admin only).",
            "删除班级职位（仅辅导员可用）。",
        ),
        json!({
            "type": "object",
            "properties": {
                "role_id": {"type": "integer", "description": t("Role id.", "职位 ID。")}
            },
            "required": ["role_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_19.clone();
            async move { role_delete(args, paths).await }
        },
    ));

    let paths_for_tool_20 = paths.clone();
    registry.register(ToolSpec::new(
        "role_query",
        t(
            "List class officer roles, optionally filtered by class.",
            "列出班级职位（可按班级过滤）。",
        ),
        json!({
            "type": "object",
            "properties": {
                "class_name": {"type": "string", "description": t("Filter by class name.", "按班级名称过滤。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_20.clone();
            async move { role_query(args, paths).await }
        },
    ));

    let paths_for_tool_21 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_apply",
        t(
            "Submit a credit application (class officers only, e.g. 班长). The application stays pending until the advisor approves it. Evidence photos (base64, up to 3) are optional but recommended.",
            "提交学分申报（仅班级职位人员可用，如 班长）。申报为待审批状态，辅导员通过后才计入学分。证据照片（base64，最多 3 张）可选但建议提供。",
        ),
        json!({
            "type": "object",
            "properties": {
                "type_name": {"type": "string", "description": t("Credit type name (e.g. 志愿公益).", "学分类型名称（如 志愿公益）。")},
                "points": {"type": "number", "description": t("Points applied for (positive).", "申报分值（正数）。")},
                "description": {"type": "string", "description": t("What happened, when, organizer, etc.", "事项说明：活动内容、时间、组织方等。")},
                "evidence": {"type": "array", "description": t("Evidence photos as base64: [{\"name\": \"a.jpg\", \"data\": \"<base64>\"}]. Max 3, each ~1MB.", "证据照片 base64 数组：[{\"name\":\"a.jpg\",\"data\":\"<base64>\"}]，最多 3 张，每张约 1MB。"), "items": {"type": "object"}}
            },
            "required": ["type_name", "points"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_21.clone();
            async move { credit_apply(args, paths).await }
        },
    ));

    let paths_for_tool_22 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_submissions_query",
        t(
            "List credit applications (admin only), filtered by status/class/date range. Each submission shows student, type, points, description, evidence files, review note.",
            "查询学分申报列表（仅辅导员可用），可按状态/班级/日期范围过滤。每条含学生、类型、分值、说明、证据文件、审批意见。",
        ),
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": t("Filter: pending / approved / rejected (empty = all).", "状态过滤：pending / approved / rejected（留空=全部）。")},
                "class_name": {"type": "string", "description": t("Filter by class name.", "按班级名称过滤。")},
                "date_from": {"type": "string", "description": t("Start date (inclusive), e.g. 2026-08-01.", "开始日期（含），如 2026-08-01。")},
                "date_to": {"type": "string", "description": t("End date (inclusive), e.g. 2026-08-31.", "结束日期（含），如 2026-08-31。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_22.clone();
            async move { credit_submissions_query(args, paths).await }
        },
    ));

    let paths_for_tool_23 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_approve",
        t(
            "Approve a pending credit application (admin only): the points become an official credit record.",
            "通过待审批的学分申报（仅辅导员可用）：分值计入正式学分记录。",
        ),
        json!({
            "type": "object",
            "properties": {
                "submission_id": {"type": "integer", "description": t("Application id.", "申报 ID。")},
                "review_note": {"type": "string", "description": t("Optional review note.", "审批意见（可选）。")}
            },
            "required": ["submission_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_23.clone();
            async move { credit_approve(args, paths).await }
        },
    ));

    let paths_for_tool_24 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_reject",
        t(
            "Reject a pending credit application (admin only) with a reason.",
            "驳回待审批的学分申报（仅辅导员可用），需附理由。",
        ),
        json!({
            "type": "object",
            "properties": {
                "submission_id": {"type": "integer", "description": t("Application id.", "申报 ID。")},
                "review_note": {"type": "string", "description": t("Rejection reason.", "驳回理由。")}
            },
            "required": ["submission_id"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_24.clone();
            async move { credit_reject(args, paths).await }
        },
    ));

    let paths_for_tool_25 = paths.clone();
    registry.register(ToolSpec::new(
        "credit_submissions_summary",
        t(
            "Summarize credit applications (admin only) in a date range, grouped by class and credit type. Use this when the advisor asks to summarize today's/recent submissions.",
            "汇总某一日期区间的学分申报（仅辅导员可用），按班级与学分类型分组。辅导员问\"总结今天/最近的提交情况\"时调用此工具。",
        ),
        json!({
            "type": "object",
            "properties": {
                "date_from": {"type": "string", "description": t("Start date (inclusive), e.g. 2026-08-01. Default: today.", "开始日期（含），如 2026-08-01。默认今天。")},
                "date_to": {"type": "string", "description": t("End date (inclusive), e.g. 2026-08-31. Default: today.", "结束日期（含），如 2026-08-31。默认今天。")}
            },
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths_for_tool_25.clone();
            async move { credit_submissions_summary(args, paths).await }
        },
    ));
}

// ─────────────────────────── 班级职位 / 问卷申报 / 审批 ───────────────────────────

/// 证据照片落盘（工具与直连/中继 REST 共用）：data_dir/evidence/<id>_<n>.<ext>。
pub(crate) fn save_evidence_files(
    paths: &GqyPaths,
    submission_id: i64,
    evidence: &[Value],
) -> Result<Vec<Value>> {
    let evidence_dir = paths.data_dir.join("evidence");
    std::fs::create_dir_all(&evidence_dir)?;
    let mut saved = Vec::new();
    for (index, item) in evidence.iter().enumerate().take(3) {
        let name = item.get("name").and_then(Value::as_str).unwrap_or("photo");
        let data = item.get("data").and_then(Value::as_str).unwrap_or("");
        if data.is_empty() {
            continue;
        }
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|error| anyhow::anyhow!("图片解码失败：{error}"))?;
        let extension = std::path::Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg")
            .to_ascii_lowercase();
        let file_name = format!("{submission_id}_{}.{extension}", index + 1);
        std::fs::write(evidence_dir.join(&file_name), bytes)?;
        saved.push(json!({"name": name, "file": file_name}));
    }
    Ok(saved)
}

async fn role_add(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let class_name = match arg_str(&args, "class_name") {
        Some(value) => value.to_string(),
        None => return Ok(fail("需要提供班级名称（class_name）")),
    };
    let title = match arg_str(&args, "title") {
        Some(value) => value.to_string(),
        None => return Ok(fail("需要提供职位名称（title）")),
    };
    let Some((class_id, _)) = db.find_class_by_name(&class_name)? else {
        return Ok(fail(&format!("班级 {class_name} 不存在")));
    };
    let student_id = match arg_str(&args, "student_no") {
        Some(no) => match db.find_student_by_no(no)? {
            Some(student) => Some(student.id),
            None => return Ok(fail(&format!("学号 {no} 不存在（可先 student_add 建档）"))),
        },
        None => None,
    };
    let note = arg_str(&args, "note").unwrap_or("").to_string();
    match db.role_add(class_id, &title, student_id, &note) {
        Ok(id) => {
            let bound = match student_id {
                Some(sid) => match db.find_student_by_id(sid)? {
                    Some(student) => format!("{}（{}）", student.name, student.student_no),
                    None => "（未绑定）".to_string(),
                },
                None => "（未绑定）".to_string(),
            };
            Ok(ok(format!(
                "已新增职位「{title}」（{class_name}）→ {bound}（职位 ID={id}）"
            )))
        }
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn role_update(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let role_id = match args.get("role_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要提供职位 ID（role_id）")),
    };
    let title = arg_str(&args, "title");
    // student_no 存在且为空串 → 解绑；非空 → 换绑；缺失 → 不变
    let student_id = match args.get("student_no") {
        Some(Value::String(value)) if value.trim().is_empty() => Some(None),
        Some(Value::String(value)) => match db.find_student_by_no(value)? {
            Some(student) => Some(Some(student.id)),
            None => return Ok(fail(&format!("学号 {} 不存在", value.trim()))),
        },
        _ => None,
    };
    let note = arg_str(&args, "note");
    if title.is_none() && student_id.is_none() && note.is_none() {
        return Ok(fail("没有要修改的内容（title / student_no / note 至少一个）"));
    }
    match db.role_update(role_id, title, student_id, note) {
        Ok(()) => Ok(ok(format!("职位已更新（ID={role_id}）"))),
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn role_delete(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let role_id = match args.get("role_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要提供职位 ID（role_id）")),
    };
    let db = open_db(&paths)?;
    match db.role_delete(role_id) {
        Ok(()) => Ok(ok(format!("职位已删除（ID={role_id}）"))),
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn role_query(args: Value, paths: GqyPaths) -> Result<String> {
    let db = open_db(&paths)?;
    let class_id = match arg_str(&args, "class_name") {
        Some(name) => match db.find_class_by_name(name)? {
            Some((id, _)) => Some(id),
            None => return Ok(fail(&format!("班级 {name} 不存在"))),
        },
        None => None,
    };
    let roles = db.list_roles(class_id)?;
    if roles.is_empty() {
        return Ok(ok("当前没有班级职位。".to_string()));
    }
    let mut lines = Vec::new();
    for role_row in roles {
        let bound = match (&role_row.student_no, &role_row.student_name) {
            (Some(no), Some(name)) => format!("{name}（{no}）"),
            _ => "（未绑定）".to_string(),
        };
        lines.push(format!(
            "- {} / {}：{bound}（ID={}）{}",
            role_row.class_name,
            role_row.title,
            role_row.id,
            if role_row.note.is_empty() {
                String::new()
            } else {
                format!(" · {}", role_row.note)
            }
        ));
    }
    Ok(ok(lines.join("\n")))
}

async fn credit_apply(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_officer(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    // 职位人员：本人申报；管理员：可替指定学生申报（需 student_no）
    let student = match &role {
        Role::Officer { student, .. } => student.clone(),
        Role::Admin => {
            let student = match student_from_args(&db, &args) {
                Ok(student) => student,
                Err(err) => return Ok(fail(&err.to_string())),
            };
            student
        }
        _ => return Ok(fail("只有班级职位人员（如班长）才能填写学分申报")),
    };
    let points = match args.get("points").and_then(Value::as_f64) {
        Some(value) if value > 0.0 => value,
        _ => return Ok(fail("points 必须是正数（申报加分）")),
    };
    let type_id = match type_id_from_args(&db, &args) {
        Ok(id) => id,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let description = arg_str(&args, "description").unwrap_or("").to_string();
    let evidence: Vec<Value> = args
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let submission_id = match db.add_submission(student.id, type_id, points, &description, "") {
        Ok(id) => id,
        Err(err) => return Ok(fail(&err.to_string())),
    };
    let mut saved = Vec::new();
    if !evidence.is_empty() {
        match save_evidence_files(&paths, submission_id, &evidence) {
            Ok(files) => {
                saved = files;
                if let Err(err) = db.set_submission_evidence(
                    submission_id,
                    &serde_json::to_string(&saved).unwrap_or_default(),
                ) {
                    return Ok(fail(&format!("证据记录失败：{err}")));
                }
            }
            Err(err) => return Ok(fail(&format!("证据保存失败：{err}"))),
        }
    }
    let type_name = match type_id {
        Some(id) => db
            .find_credit_type_by_id(id)
            .ok()
            .flatten()
            .map(|(_, name, _)| name)
            .unwrap_or_default(),
        None => String::new(),
    };
    let student_name = student.name.clone();
    let student_no = student.student_no.clone();
    Ok(ok(format!(
        "申报已提交（{student_name} 学号 {student_no}）：{type_name} +{points} 分{}\n状态：待辅导员审批{}",
        if description.is_empty() { String::new() } else { format!("（{description}）") },
        if saved.is_empty() { String::new() } else { format!("，已附 {} 张证据照片", saved.len()) }
    )))
}

async fn credit_submissions_query(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let db = open_db(&paths)?;
    let status = arg_str(&args, "status").map(str::to_string);
    let class_id = match arg_str(&args, "class_name") {
        Some(name) => match db.find_class_by_name(name)? {
            Some((id, _)) => Some(id),
            None => return Ok(fail(&format!("班级 {name} 不存在"))),
        },
        None => None,
    };
    let date_from = arg_str(&args, "date_from").map(|d| format!("{d}T00:00:00Z"));
    let date_to = arg_str(&args, "date_to").map(|d| format!("{d}T23:59:59Z"));
    let submissions = db.list_submissions(
        status.as_deref(),
        class_id,
        None,
        date_from.as_deref(),
        date_to.as_deref(),
    )?;
    if submissions.is_empty() {
        return Ok(ok("没有符合条件的学分申报。".to_string()));
    }
    let status_label = |status: &str| match status {
        "approved" => "✅ 已通过",
        "rejected" => "❌ 已驳回",
        _ => "⏳ 待审批",
    };
    let mut lines = Vec::new();
    for submission in submissions {
        let class = submission.class_name.as_deref().unwrap_or("未分班");
        let type_name = submission.type_name.as_deref().unwrap_or("未分类");
        let mut line = format!(
            "#{} {}（{}）{}班：{type_name} +{} 分 {}",
            submission.id,
            submission.student_name,
            submission.student_no,
            class,
            submission.points,
            status_label(&submission.status)
        );
        if !submission.description.is_empty() {
            line.push_str(&format!("｜{}", submission.description));
        }
        if submission.status != "pending" && !submission.review_note.is_empty() {
            line.push_str(&format!("｜意见：{}", submission.review_note));
        }
        lines.push(line);
    }
    Ok(ok(format!(
        "共 {} 条申报：\n{}",
        lines.len(),
        lines.join("\n")
    )))
}

async fn credit_approve(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let submission_id = match args.get("submission_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要提供申报 ID（submission_id）")),
    };
    let review_note = arg_str(&args, "review_note").unwrap_or("").to_string();
    let db = open_db(&paths)?;
    match db.approve_submission(submission_id, &review_note, "辅导员") {
        Ok(()) => {
            let submission = db.find_submission(submission_id)?.unwrap_or_else(|| {
                panic!("approve 成功但查不到申报 {submission_id}")
            });
            let type_name = submission.type_name.as_deref().unwrap_or("").to_string();
            Ok(ok(format!(
                "已通过申报 #{}：{}（{}）{} +{} 分 → 已计入正式学分",
                submission.id,
                submission.student_name,
                submission.student_no,
                type_name,
                submission.points,
            )))
        }
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn credit_reject(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let submission_id = match args.get("submission_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return Ok(fail("需要提供申报 ID（submission_id）")),
    };
    let review_note = arg_str(&args, "review_note").unwrap_or("").to_string();
    let db = open_db(&paths)?;
    match db.reject_submission(submission_id, &review_note) {
        Ok(()) => Ok(ok(format!(
            "已驳回申报 #{submission_id}{}",
            if review_note.is_empty() {
                String::new()
            } else {
                format!("（理由：{review_note}）")
            }
        ))),
        Err(err) => Ok(fail(&err.to_string())),
    }
}

async fn credit_submissions_summary(args: Value, paths: GqyPaths) -> Result<String> {
    let role = current_role(&paths);
    if let Err(err) = require_admin(&role) {
        return Ok(fail(&err.to_string()));
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let date_from = arg_str(&args, "date_from")
        .unwrap_or(&today)
        .to_string();
    let date_to = arg_str(&args, "date_to").unwrap_or(&today).to_string();
    let db = open_db(&paths)?;
    let summary = db.submissions_summary(
        Some(&format!("{date_from}T00:00:00Z")),
        Some(&format!("{date_to}T23:59:59Z")),
    )?;
    if summary.is_empty() {
        return Ok(ok(format!(
            "{date_from} 至 {date_to} 没有学分申报记录。"
        )));
    }
    let mut lines = vec![format!("{date_from} 至 {date_to} 学分申报汇总：")];
    let mut totals = (0i64, 0i64, 0i64, 0.0f64);
    for row in &summary {
        let class = row.class_name.as_deref().unwrap_or("未分班");
        let type_name = row.type_name.as_deref().unwrap_or("未分类");
        lines.push(format!(
            "- {class}｜{type_name}：待审批 {} 条、已通过 {} 条、已驳回 {} 条、通过学分 {} 分",
            row.pending, row.approved, row.rejected, row.total_points
        ));
        totals.0 += row.pending;
        totals.1 += row.approved;
        totals.2 += row.rejected;
        totals.3 += row.total_points;
    }
    lines.push(format!(
        "合计：待审批 {} 条、已通过 {} 条、已驳回 {} 条、通过学分 {} 分",
        totals.0, totals.1, totals.2, totals.3
    ));
    if totals.0 > 0 {
        lines.push("提示：还有待审批申报，可在面板「学分管理 → 申报审批」处理，或让我（AI）逐个查看。".to_string());
    }
    Ok(ok(lines.join("\n")))
}
