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
    match db.find_student_by_platform(&identity.platform, &identity.user_id) {
        Ok(Some(student)) => Role::Student(Some(student)),
        _ => Role::Student(None),
    }
}

fn require_admin(role: &Role) -> Result<()> {
    match role {
        Role::Admin => Ok(()),
        _ => bail!("该操作需要管理员（辅导员）权限；学生只能查询自己的信息"),
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
    let operator = current_operator(&paths);
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

fn current_operator(paths: &GqyPaths) -> String {
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
        Role::Student(Some(student)) => {
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
        Role::Student(Some(student)) => {
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
}
