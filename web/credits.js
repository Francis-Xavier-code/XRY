// 希尔娅 · 学分管理页（辅导员专用，与面板共用会话认证）
'use strict';

const $ = (id) => document.getElementById(id);

let state = {
  classes: [],
  types: [],
  students: [],
  records: [],
  activeClassId: null,
};

async function api(method, url, body) {
  const options = { method, headers: {} };
  if (body !== undefined) {
    options.headers['Content-Type'] = 'application/json';
    options.body = JSON.stringify(body);
  }
  const res = await fetch(url, options);
  if (res.status === 401) {
    location.href = '/';
    throw new Error('未登录');
  }
  if (!res.ok) {
    let message = '请求失败';
    try {
      const data = await res.json();
      message = data.error || message;
    } catch (_) { /* ignore */ }
    throw new Error(message);
  }
  return res.json();
}

// ───────────────────────── 数据加载 ─────────────────────────

async function loadOverview() {
  const data = await api('GET', '/api/credits/overview');
  state.classes = data.classes || [];
  state.types = data.types || [];
  $('statClasses').textContent = data.total_classes ?? state.classes.length;
  $('statStudents').textContent = data.total_students ?? 0;
  $('statRecords').textContent = data.total_records ?? 0;
  $('statPoints').textContent = data.total_points ?? 0;
  renderClasses();
  renderTypes();
  renderRecordFilters();
}

async function loadStudents() {
  const params = new URLSearchParams();
  if (state.activeClassId) params.set('class_id', state.activeClassId);
  const keyword = $('studentKeyword').value.trim();
  if (keyword) params.set('keyword', keyword);
  const data = await api('GET', '/api/credits/students?' + params.toString());
  state.students = data.students || [];
  renderStudents();
}

async function loadRecords() {
  const params = new URLSearchParams();
  if (state.activeClassId) params.set('class_id', state.activeClassId);
  const keyword = $('recordKeyword').value.trim();
  if (keyword) params.set('keyword', keyword);
  const typeId = $('recordTypeFilter').value;
  if (typeId) params.set('type_id', typeId);
  const semester = $('recordSemester').value.trim();
  if (semester) params.set('semester', semester);
  const data = await api('GET', '/api/credits/records?' + params.toString());
  state.records = data.records || [];
  renderRecords();
}

// ───────────────────────── 渲染 ─────────────────────────

function renderClasses() {
  $('classCount').textContent = `${state.classes.length} 个`;
  const list = $('classList');
  list.innerHTML = '';
  if (state.classes.length === 0) {
    list.innerHTML = '<div class="empty">还没有班级</div>';
  }
  for (const cls of state.classes) {
    const item = document.createElement('div');
    item.className = 'class-item' + (state.activeClassId === cls.id ? ' active' : '');
    const name = document.createElement('span');
    name.textContent = cls.name;
    const n = document.createElement('span');
    n.className = 'n';
    n.textContent = `${cls.student_count}人`;
    item.append(name, n);
    item.onclick = () => {
      state.activeClassId = state.activeClassId === cls.id ? null : cls.id;
      renderClasses();
      loadStudents();
      loadRecords();
    };
    item.ondblclick = (e) => {
      e.stopPropagation();
      openDialog('editClass', cls);
    };
    list.appendChild(item);
  }
}

function renderTypes() {
  $('typeCount').textContent = `${state.types.length} 个`;
  const list = $('typeList');
  list.innerHTML = '';
  for (const type of state.types) {
    const item = document.createElement('div');
    item.className = 'type-item';
    const name = document.createElement('span');
    name.textContent = type.name;
    const limit = document.createElement('span');
    limit.textContent = type.max_points > 0 ? `上限${type.max_points}` : '';
    item.append(name, limit);
    list.appendChild(item);
  }
}

function renderRecordFilters() {
  const classFilter = $('recordClassFilter');
  const prevClass = classFilter.value;
  classFilter.innerHTML = '<option value="">全部班级</option>';
  for (const cls of state.classes) {
    const opt = document.createElement('option');
    opt.value = cls.id;
    opt.textContent = cls.name;
    classFilter.appendChild(opt);
  }
  classFilter.value = prevClass;
  const typeFilter = $('recordTypeFilter');
  const prevType = typeFilter.value;
  typeFilter.innerHTML = '<option value="">全部类型</option>';
  for (const type of state.types) {
    const opt = document.createElement('option');
    opt.value = type.id;
    opt.textContent = type.name;
    typeFilter.appendChild(opt);
  }
  typeFilter.value = prevType;
}

function renderStudents() {
  const body = $('studentBody');
  body.innerHTML = '';
  if (state.students.length === 0) {
    body.innerHTML = '<tr><td colspan="7" class="empty">没有匹配的学生</td></tr>';
    return;
  }
  for (const s of state.students) {
    const tr = document.createElement('tr');
    tr.innerHTML = `
      <td>${esc(s.student_no)}</td>
      <td>${esc(s.name)}</td>
      <td>${esc(s.class_name || '未分班')}</td>
      <td>${esc(s.gender || '')}</td>
      <td>${esc(s.phone || '')}</td>
      <td class="${s.total_points >= 0 ? 'points-pos' : 'points-neg'}">${s.total_points}</td>
      <td class="row-actions">
        <button data-act="edit">编辑</button>
        <button data-act="records">记录</button>
        <button data-act="del" class="danger">删除</button>
      </td>`;
    tr.querySelector('[data-act="edit"]').onclick = () => openDialog('editStudent', s);
    tr.querySelector('[data-act="records"]').onclick = () => {
      state.activeClassId = null;
      renderClasses();
      $('recordKeyword').value = s.student_no;
      loadRecords();
    };
    tr.querySelector('[data-act="del"]').onclick = async () => {
      if (!confirm(`确定删除学生 ${s.name}（${s.student_no}）及其全部学分记录？此操作不可恢复。`)) return;
      try {
        await api('DELETE', `/api/credits/students/${s.id}`);
        await loadOverview();
        await loadStudents();
        await loadRecords();
      } catch (e) {
        alert(e.message);
      }
    };
    body.appendChild(tr);
  }
}

function renderRecords() {
  const body = $('recordBody');
  body.innerHTML = '';
  if (state.records.length === 0) {
    body.innerHTML = '<tr><td colspan="8" class="empty">没有匹配的学分记录</td></tr>';
    return;
  }
  for (const r of state.records) {
    const tr = document.createElement('tr');
    const cls = r.points >= 0 ? 'points-pos' : 'points-neg';
    const sign = r.points >= 0 ? '+' : '';
    tr.innerHTML = `
      <td>${esc(r.student_name)}（${esc(r.student_no)}）</td>
      <td>${esc(r.type_name || '未分类')}</td>
      <td class="${cls}">${sign}${r.points}</td>
      <td>${esc(r.semester || '')}</td>
      <td>${esc(r.happened_on || '')}</td>
      <td>${esc(r.note || '')}</td>
      <td>${esc(r.operator || '')}</td>
      <td class="row-actions">
        <button data-act="edit">编辑</button>
        <button data-act="del" class="danger">删除</button>
      </td>`;
    tr.querySelector('[data-act="edit"]').onclick = () => openDialog('editRecord', r);
    tr.querySelector('[data-act="del"]').onclick = async () => {
      if (!confirm(`确定删除记录 #${r.id}（${r.student_name} ${r.type_name || ''} ${r.points} 分）？`)) return;
      try {
        await api('DELETE', `/api/credits/records/${r.id}`);
        await loadOverview();
        await loadRecords();
        await loadStudents();
      } catch (e) {
        alert(e.message);
      }
    };
    body.appendChild(tr);
  }
}

function esc(text) {
  return String(text).replace(/[&<>"']/g, (ch) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[ch]);
}

// ───────────────────────── 对话框 ─────────────────────────

function openDialog(kind, item) {
  const root = $('dialogRoot');
  let html = '';
  let title = '';
  if (kind === 'addStudent' || kind === 'editStudent') {
    const s = item || {};
    title = kind === 'addStudent' ? '添加学生' : `编辑学生 ${s.name || ''}`;
    html = `
      <div class="field"><label>学号 *</label><input id="fStudentNo" value="${esc(s.student_no || '')}"></div>
      <div class="field"><label>姓名 *</label><input id="fStudentName" value="${esc(s.name || '')}"></div>
      <div class="field"><label>班级</label><select id="fStudentClass">${classOptions(s.class_id)}</select></div>
      <div class="field"><label>性别</label><input id="fStudentGender" value="${esc(s.gender || '')}"></div>
      <div class="field"><label>电话</label><input id="fStudentPhone" value="${esc(s.phone || '')}"></div>
      <div class="field"><label>备注</label><input id="fStudentNote" value="${esc(s.note || '')}"></div>`;
  } else if (kind === 'addRecord' || kind === 'editRecord') {
    const r = item || {};
    title = kind === 'addRecord' ? '记录学分' : `编辑记录 #${r.id || ''}`;
    html = `
      <div class="field"><label>学生</label><select id="fRecordStudent">${studentOptions(r.student_id)}</select></div>
      <div class="field"><label>学分类型</label><select id="fRecordType">${typeOptions(r.type_id)}</select></div>
      <div class="field"><label>分值（加分正数 / 扣分负数）*</label><input id="fRecordPoints" type="number" step="0.1" value="${r.points ?? ''}"></div>
      <div class="field"><label>学期</label><input id="fRecordSemester" value="${esc(r.semester || '')}" placeholder="如 2025-2026-1"></div>
      <div class="field"><label>日期</label><input id="fRecordDate" value="${esc(r.happened_on || '')}" placeholder="如 2026-03-10"></div>
      <div class="field"><label>备注</label><input id="fRecordNote" value="${esc(r.note || '')}"></div>`;
  } else if (kind === 'addClass' || kind === 'editClass') {
    const c = item || {};
    title = kind === 'addClass' ? '新建班级' : `编辑班级 ${c.name || ''}`;
    html = `
      <div class="field"><label>班级名称 *</label><input id="fClassName" value="${esc(c.name || '')}"></div>
      <div class="field"><label>年级</label><input id="fClassGrade" value="${esc(c.grade || '')}"></div>
      <div class="field"><label>专业</label><input id="fClassMajor" value="${esc(c.major || '')}"></div>
      <div class="field"><label>备注</label><input id="fClassNote" value="${esc(c.note || '')}"></div>`;
  } else if (kind === 'addType') {
    title = '添加学分类型';
    html = `
      <div class="field"><label>类型名称 *</label><input id="fTypeName"></div>
      <div class="field"><label>说明</label><input id="fTypeDesc"></div>
      <div class="field"><label>每人上限（0 = 不限）</label><input id="fTypeMax" type="number" step="0.5" value="0"></div>`;
  } else if (kind === 'importCsv') {
    title = 'CSV 批量导入学生';
    html = `
      <p class="hint">每行一个学生：<b>学号,姓名,班级,性别,电话</b>（班级不存在时自动创建）。示例：</p>
      <textarea id="fCsv">2023010101,张三,计科2301,男,13800000000
2023010102,李四,计科2301,女,13900000000</textarea>`;
  }

  const overlay = document.createElement('div');
  overlay.className = 'dialog-overlay';
  overlay.innerHTML = `
    <div class="dialog">
      <h3>${title}</h3>
      ${html}
      <div class="dialog-actions">
        <button class="cbtn secondary" id="dialogCancel">取消</button>
        <button class="cbtn" id="dialogOk">保存</button>
      </div>
    </div>`;
  root.appendChild(overlay);
  overlay.onclick = (e) => { if (e.target === overlay) overlay.remove(); };
  $('dialogCancel').onclick = () => overlay.remove();
  $('dialogOk').onclick = async () => {
    try {
      if (kind === 'addStudent' || kind === 'editStudent') {
        const payload = {
          student_no: $('fStudentNo').value.trim(),
          name: $('fStudentName').value.trim(),
          class_id: $('fStudentClass').value ? Number($('fStudentClass').value) : null,
          gender: $('fStudentGender').value.trim(),
          phone: $('fStudentPhone').value.trim(),
          note: $('fStudentNote').value.trim(),
        };
        if (kind === 'addStudent') {
          await api('POST', '/api/credits/students', payload);
        } else {
          await api('PUT', `/api/credits/students/${item.id}`, payload);
        }
      } else if (kind === 'addRecord' || kind === 'editRecord') {
        const payload = {
          student_id: Number($('fRecordStudent').value),
          type_id: $('fRecordType').value ? Number($('fRecordType').value) : null,
          points: Number($('fRecordPoints').value),
          semester: $('fRecordSemester').value.trim(),
          happened_on: $('fRecordDate').value.trim(),
          note: $('fRecordNote').value.trim(),
        };
        if (!payload.student_id || !payload.points) throw new Error('请选择学生并填写分值');
        if (kind === 'addRecord') {
          await api('POST', '/api/credits/records', payload);
        } else {
          await api('PUT', `/api/credits/records/${item.id}`, payload);
        }
      } else if (kind === 'addClass' || kind === 'editClass') {
        const payload = {
          name: $('fClassName').value.trim(),
          grade: $('fClassGrade').value.trim(),
          major: $('fClassMajor').value.trim(),
          note: $('fClassNote').value.trim(),
        };
        if (kind === 'addClass') {
          await api('POST', '/api/credits/classes', payload);
        } else {
          await api('PUT', `/api/credits/classes/${item.id}`, payload);
        }
      } else if (kind === 'addType') {
        await api('POST', '/api/credits/types', {
          name: $('fTypeName').value.trim(),
          description: $('fTypeDesc').value.trim(),
          max_points: Number($('fTypeMax').value || 0),
        });
      } else if (kind === 'importCsv') {
        const res = await api('POST', '/api/credits/import', { csv: $('fCsv').value });
        alert(res.message || '导入完成');
      }
      overlay.remove();
      await refreshAll();
    } catch (e) {
      alert(e.message);
    }
  };
}

function classOptions(selected) {
  return '<option value="">未分班</option>' + state.classes
    .map((c) => `<option value="${c.id}" ${c.id === selected ? 'selected' : ''}>${esc(c.name)}</option>`)
    .join('');
}

function typeOptions(selected) {
  return '<option value="">未分类</option>' + state.types
    .map((t) => `<option value="${t.id}" ${t.id === selected ? 'selected' : ''}>${esc(t.name)}</option>`)
    .join('');
}

function studentOptions(selected) {
  const students = state.students.length ? state.students : [{ id: 0, student_no: '', name: '' }];
  return students
    .map((s) => `<option value="${s.id}" ${s.id === selected ? 'selected' : ''}>${esc(s.student_no)} ${esc(s.name)}</option>`)
    .join('');
}

async function refreshAll() {
  await loadOverview();
  await loadStudents();
  await loadRecords();
}

// ───────────────────────── 事件绑定 ─────────────────────────

$('addClassBtn').onclick = () => openDialog('addClass');
$('addTypeBtn').onclick = () => openDialog('addType');
$('addStudentBtn').onclick = () => openDialog('addStudent');
$('addRecordBtn').onclick = () => openDialog('addRecord');
$('importCsvBtn').onclick = () => openDialog('importCsv');

let keywordTimer = null;
$('studentKeyword').oninput = () => { clearTimeout(keywordTimer); keywordTimer = setTimeout(loadStudents, 300); };
$('recordKeyword').oninput = () => { clearTimeout(keywordTimer); keywordTimer = setTimeout(loadRecords, 300); };
$('recordClassFilter').onchange = () => {
  const value = $('recordClassFilter').value;
  if (value) {
    state.activeClassId = null;
    renderClasses();
    $('recordKeyword').value = '';
  }
  loadRecords();
};
$('recordTypeFilter').onchange = loadRecords;
$('recordSemester').oninput = () => { clearTimeout(keywordTimer); keywordTimer = setTimeout(loadRecords, 300); };

refreshAll().catch((e) => {
  if (e.message === '未登录') return;
  alert('加载失败：' + e.message);
});
