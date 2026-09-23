// Read-only source fetch. Writes ONLY this candidate directory; never activates rules.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {prepareAssistantRequest} from '../../mvp/adapters/assistant.mjs';
const dir=path.dirname(fileURLToPath(import.meta.url));
const read=n=>JSON.parse(fs.readFileSync(path.join(dir,n),'utf8'));
const write=(n,v)=>fs.writeFileSync(path.join(dir,n),typeof v==='string'?v:JSON.stringify(v,null,2)+'\n');
const hash=s=>crypto.createHash('sha256').update(s,'utf8').digest('hex');
const core=read('authored-core.json'), ed=read('editorial-analysis.json');
const data=await(await fetch('http://127.0.0.1:4199/api/knowledge',{signal:AbortSignal.timeout(30000)})).json();
const active=data.entries.filter(e=>e.kind==='rule'&&e.status==='active'&&e.scope?.account==='LikeAvto').map(e=>({e,v:data.versions.find(v=>v.id===e.currentVersionId)}));
assert.equal(active.length,31,'Active source set changed; review before rebuilding');
const scope={account:'LikeAvto',postKeys:[]};
const sourceOrder=[['G00','Общий стиль: стиль'],...[1,2,3,4,5,6,7,8,9,10,11,12,14,15,16,17,18,19].map(n=>[`G${String(n).padStart(2,'0')}`,`Общий стиль: правило ${n}`]),['L00','LikeAvto: стиль'],...[1,3,4,5,6,7].map(n=>[`L${String(n).padStart(2,'0')}`,`LikeAvto: правило ${n}`])];
const selected=new Map();
for(const [id,title] of sourceOrder){const found=active.filter(x=>x.v.title===title);assert.equal(found.length,1,title);selected.set(id,found[0]);}
for(const s of ed.sources){const found=active.find(x=>x.e.id===s.entryId);assert(found&&found.v.id===s.versionId&&hash(found.v.text)===s.textSha256Utf8,'Editorial source changed');selected.set(s.symbol,found);}
const constraintDefs=[
 {id:'LA-C01',sourceId:'C_FORBIDDEN_SUBSTRINGS',entryId:'knowledge-16d963fe5e4aa4bbf63d7145e21271040c0d8be84bcab18cd03342a5dd551441',category:'Запрещённые упоминания',operator:{effect:'deny',subject:'reply_text',operation:'literal_substring',normalization:'Python str.lower on reply and values'},values:['bawrussia.ru','bawofficial.ru','baw-official.ru','t.me/bawrussia','t.me/baw_support','t.me/bawrussia_sales_bot','бав россия','baw russia'],when:'Проверяешь публичный текст LikeAvto.',action:'Запрети буквальную подстроку из списка без учёта регистра; это не регулярные выражения.'},
 {id:'LA-C02',sourceId:'C_ALLOWED_URLS',entryId:'knowledge-6eed4715e716260a0e4ade0fa85a025cb6447a49513bab446186573bebb71bd9',category:'Разрешённые URL',operator:{effect:'allow_only',subject:'reply_urls',operation:'exact_url',extractionRegex:'https?://[^\\s<>]+',stripTrailingCharacters:'.,!?;:)]}',caseSensitive:true},values:['https://likeavto.ru/','https://likeavto.ru/calc','https://t.me/likeavto_op_bot','https://max.ru/id753613695767_biz','https://vk.com/likeavto_import'],when:'В публичном ответе есть HTTP(S)-ссылки.',action:'После извлечения и удаления конечной пунктуации допускай только точное совпадение со списком; разрешение домена не подразумевается.'},
 {id:'LA-C03',sourceId:'C_FORBIDDEN_PREFIXES',entryId:'knowledge-5cd9d4c9c802e488ed5a3c77df23c11f8c85f1e5be3799e0e7fa8ce9032b3e63',category:'Запрещённое начало',operator:{effect:'deny',subject:'reply_text',operation:'literal_prefix',normalization:'Python str.strip().casefold() on reply; str.casefold() on value'},values:['спасибо за комментарий'],when:'Проверяешь начало публичного ответа.',action:'Запрети перечисленный префикс после удаления краевых пробелов и casefold; совпадение внутри текста не является префиксом.'}
];
for(const c of constraintDefs){const found=active.find(x=>x.e.id===c.entryId);assert(found);assert.deepEqual(JSON.parse(found.v.text),c.values);selected.set(c.sourceId,found);}
assert.equal(selected.size,31);
const sources=[...selected].map(([id,{e,v}])=>({id,entryId:e.id,versionId:v.id,versionHash:v.hash,sourceHash:v.sourceHash,textSha256Utf8:hash(v.text),textLengthUtf16:v.text.length,textBytesUtf8:Buffer.byteLength(v.text),scope:e.scope,trust:v.trust,category:v.category,sourceMaterialId:v.sourceMaterialId,disposition:id.startsWith('C_')?'retain':'replace'}));
assert(sources.every(s=>s.trust==='imported_policy'&&JSON.stringify(s.scope)===JSON.stringify(scope)));
const rules=structuredClone(core.rules);
const R=id=>rules.find(r=>r.id===id);
R('LA-S02').action='Сохрани одну главную мысль. Обычно 1–3 коротких предложения; старайся уложиться в 1–2, если смысл сохранён.';
R('LA-S02').exceptions[0]='Для содержательного вопроса можно больше.';
R('LA-A01').action='Активно ищи уместный короткий ответ: тёплую реакцию, шутку, полезную конкретику или следующий шаг.';
R('LA-A01').exceptions=['Оставляй без ответа, только когда он действительно пустой, повторяет уже сказанное или возобновляет исчерпанный разговор. Не задавай механический вопрос ради ответа.'];
R('LA-A02').action='Сначала выбери реакцию на точную мысль, затем слова. Ответь на заданный вопрос и содержательную претензию, не на соседнюю тему или эмоциональную оболочку.';
R('LA-A03').exceptions.push('Личное предпочтение другой машины без запроса консультации не превращай в опрос бюджета и критериев.');
R('LA-A04').when='Выбираешь следующий ответ с учётом истории.';
R('LA-A04').action='Учти опубликованные и подготовленные ready-ответы; не повторяй ответ, признание, обещание или консультацию со ссылками и не возобновляй исчерпанный спор.';
R('LA-A04').exceptions.push('После благодарности за уже данный ответ достаточно краткой вежливой реакции.');
R('LA-A09').action='Проверь дату публикации. Если возраст установлен, кратко поясни, что цена относилась к моменту публикации и могла измениться.';
R('LA-A09').exceptions=R('LA-A09').exceptions.slice(0,2);
R('LA-A09').ambiguity='AMB-CTA';
R('LA-A15').exceptions=[];
rules.splice(rules.findIndex(r=>r.id==='LA-A15'),1);
R('LA-A16').when='Вопрос или неясность касаются стоимости, расчёта, наличия, подбора или конкретных вариантов.';
R('LA-A16').action='Дай известную полезную информацию и направь за расчётом конкретного варианта в отдел продаж по проверенному маршруту аккаунта и площадки.';
R('LA-A16').exceptions=['Не выдумывай цену, наличие или смету.','Если конкретный ответ уже подтверждён и неясности нет, дополнительный CTA не обязателен.','Продажи не заменяют ответ на доступную характеристику или содержательное исследование возражения.'];
delete R('LA-A16').ambiguity;delete R('LA-A09').ambiguity;
R('LA-A10').exceptions.push('При повторной жалобе отвечай на новое содержание; при нехватке сведений команды оставь конкретный вопрос открытым вместо повторного запроса договора.');
R('LA-A18').exceptions.push('Жалоба после оплаты требует одного конкретного шага разбора; общая благодарность не заменяет его.');
R('LA-F01').exceptions.push('Не выдумывай подтверждение и наличие документов, преимущества аналоговых приборов, скидку или обоснование цены.');
R('LA-F02').action='Не выдумывай проверку, передачу коллегам, звонок, будущую публикацию, срок решения, закрытие претензии или другое действие команды. Подготовленный ready-ответ не является отправленным.';
R('LA-F02').exceptions.push('Участие ИИ признавай только при подтверждённом контексте бренда; иначе можно признать неудачный тон, не выдумывая внутренний процесс.');
R('LA-F03').action='Сопоставь модель, модельный год, рынок, поколение, гибридность и комплектацию; не переноси цифры между версиями. Для спорных характеристик сначала установи также число мест, затем положение сидений, методику и единицы измерения. Переиспользуй только подходящий факт.';
R('LA-F08').when='Опираешься на медиа или слова ведущего.';
R('LA-F08').action='Непросмотренное содержание не угадывай. Высказывание ведущего о характеристике или ресурсе не становится гарантией, даже после просмотра ролика.';
R('LA-O01').exceptions.push('Поясняющие скобки из редакторских примеров в публичный текст не переносятся.');
const ownerDecision={schemaVersion:1,id:'OWNER-REACTION-20260923',date:'2026-09-23',account:'LikeAvto',resolvedAmbiguityIds:['AMB-REACTION'],ownerQuote:'Да, по уместности',questionContext:'Отвечать на наблюдения и шутки, когда можно добавить уместную реакцию, без обязательного пустого ответа.',subsequentOwnerClarification:{verbatim:false,relayedBy:'/root',meaning:'Владелец хочет больше отвеченных комментариев: активно искать уместную короткую тёплую реакцию, шутку или полезное дополнение; оставлять без ответа действительно пустое, повторное или исчерпанное. Процентная квота и обязательные продажи не заданы.'},decision:'Активно искать уместный ответ; обязательного пустого ответа нет.',evidence:'Explicit owner answer and subsequent clarification relayed by parent /root in the current task; not inferred from legacy source.',changeType:'owner_clarification',trustElevation:false,grantsExecutionAuthority:false};
write('owner-decision-reaction.v1.json',ownerDecision);
const ownerDecisionRef={id:ownerDecision.id,sha256:hash(fs.readFileSync(path.join(dir,'owner-decision-reaction.v1.json'))),resolvedAmbiguityIds:ownerDecision.resolvedAmbiguityIds};
R('LA-A01').ownerDecisionIds=[ownerDecision.id];R('LA-A01').changeType='owner_clarification';
const ctaDecision={schemaVersion:1,id:'OWNER-CTA-20260923',date:'2026-09-23',account:'LikeAvto',resolvedAmbiguityIds:['AMB-CTA'],ownerQuote:'да в любой непонятной ситуации если дело касается стоимости расчета или такого - отдел продаж та менегеры с радостью предоставят расчет по конкретным вариантам',decision:'Вопросы и неясности о стоимости/расчёте конкретных вариантов направляются в продажи по маршруту аккаунта и площадки; известная полезная информация остаётся в ответе, цены не выдумываются.',evidence:'Explicit owner answer relayed by parent /root in the current task; deliberate clarification supersedes manager-indispensable-only condition.',changeType:'owner_clarification',trustElevation:false,grantsExecutionAuthority:false};
write('owner-decision-cta.v1.json',ctaDecision);
const ctaDecisionRef={id:ctaDecision.id,sha256:hash(fs.readFileSync(path.join(dir,'owner-decision-cta.v1.json'))),resolvedAmbiguityIds:ctaDecision.resolvedAmbiguityIds};
const decisionRefs=[ownerDecisionRef,ctaDecisionRef];
for(const id of ['LA-A09','LA-A16']){R(id).ownerDecisionIds=[ctaDecision.id];R(id).changeType='owner_clarification';}
const aliases={'LA-A15':'LA-A16','LA-E01':'LA-A02','LA-E03':'LA-E02','LA-E05':'LA-A04','LA-E06':'LA-F02','LA-E15':'LA-F02','LA-E16':'LA-O01','LA-E23':'LA-E02'};
const canon=id=>aliases[id]??id;
const additions=[
 ['LA-E02','factual_boundary','Спорный тезис','Автор возражает, исправляет нас или доказательств пока нет.','Проверь именно спорный тезис до согласия или опровержения; признавай только установленную неточность. Уверенный тон не доказательство; отсутствие подтверждения не опровержение.'],
 ['LA-E04','identity_constraint','История между постами','Используешь customer_cases или межпостовую историю.','Разделяй аккаунты и площадки; связывай автора только по author_id, никогда по имени.'],
 ['LA-E07','factual_boundary','Статус заказа','Клиент описывает состояние заказа.','Считай это сообщением клиента, а не установленным внутренним статусом.'],
 ['LA-E13','style_policy','Содержательная оговорка','Формулируешь ответ при неполном подтверждении.','Не заканчивай каждый ответ шаблонным «не можем проверить» или синонимом.','Прямая оговорка нужна в крайнем случае, когда без неё ответ вводит в заблуждение; можно сообщить установленное и конкретное ограничение.'],
 ['LA-E14','style_policy','Уважение выбора','Человек предпочитает другую машину.','Не обесценивай его выбор ради продажи своей.'],
 ['LA-E17','factual_boundary','Расхождения чисел','В источниках или реплике расходятся числа.','Заметь расхождение; не исправляй число молча.'],
 ['LA-E18','factual_boundary','Условия измерения','Источник не указал методику или положение сидений.','Не выводи их из величины числа, отсутствия слэша или соседней иллюстрации; не утверждай ни сложенный, ни поднятый ряд без данных.'],
 ['LA-E19','action_policy','Выбор источника','Ищешь характеристики.','Сначала ищи официальную спецификацию или руководство производителя нужного рынка, затем таблицу точной комплектации AutoHome или другого каталога.'],
 ['LA-E20','factual_boundary','Тип источника','Источник размещён на домене каталога, включая AutoHome.','Отличай таблицу параметров от блога, форума и вопросов-ответов; домен не делает последние первоисточниками.'],
 ['LA-E21','action_policy','Чтение таблицы','Изучаешь таблицу характеристик.','Проверь заголовки колонок и сноски.']
];
for(const [id,kind,category,when,action,exception]of additions)rules.push({id,kind,category,when,action,exceptions:exception?[exception]:[],strength:kind==='style_policy'?'default':'must'});
const contracts=[
 ['LA-E08','legacy_runtime_contract','Редакторский проход','Выполняется second_pass.','Можно заменить выбранную реакцию целиком, не только слова.','may'],
 ['LA-E09','legacy_runtime_contract','Редакторский проход','Перерабатываешь ответ в second_pass.','Сохрани необходимые факты, оговорки и контакты.','must'],
 ['LA-E10','legacy_runtime_contract','Редакторский проход','Исходная реакция уже уместна.','Оставь её.','must'],
 ['LA-E11','legacy_runtime_contract','Формат результата','Возвращаешь second_pass.','Верни итог в прежней JSON-схеме.','must'],
 ['LA-E12','interpretation_constraint','Статус примеров','Используешь примеры исходного руководства.','Это иллюстрации выбора реакции, не справочник характеристик и не универсальные ответы для копирования.','must'],
 ['LA-E22','evidence_output_contract','Сохранение факта','Сохраняешь факт в facts.','В claim укажи версию, единицу, условия и ограничения источника, а не голое число; приложи прямой source_url.','must'],
 ['LA-E24','legacy_runtime_contract','Запрос исследования','В прежнем контуре нужно исследование.','Источник называет механизм needs_research.','descriptive'],
 ['LA-E25','legacy_runtime_contract','Редакторская проверка фактов','Выполняется second_pass.','Проверь необоснованное согласие с возражением так же, как выдуманные факты (LA-E02, LA-F01).','must']
].map(([id,kind,category,when,action,strength])=>({id,kind,category,when,action,strength,exceptions:[],scope,trust:'imported_policy',layer:'legacy_contract',grantsExecutionAuthority:false}));
contracts.find(r=>r.id==='LA-E11').unresolvedReference='Конкретная прежняя JSON-схема этим источником не определена; не заменять текущий контракт CommunityHero.';
contracts.find(r=>r.id==='LA-E24').unresolvedReference='needs_research — имя прежнего механизма, не новая команда или право действия CommunityHero.';
const constraints=constraintDefs.map(({sourceId,entryId,...c})=>({...c,kind:'hard_constraint',strength:'must',exceptions:[],scope,trust:'imported_policy',layer:'output_validation',disposition:'retain_original_entry',retainedEntryId:entryId,sourceClauseIds:[sourceId+':all']}));
// Preserve the semantic evidence requirement; retire only the obsolete field names.
const evidenceRule=contracts.splice(contracts.findIndex(r=>r.id==='LA-E22'),1)[0];
Object.assign(evidenceRule,{kind:'factual_boundary',category:'Содержание сохраняемого факта',when:'Сохраняешь результат проверки для повторного использования.',action:'Сохрани версию, единицу, условия и ограничения источника вместе с прямой ссылкой, а не голое число.',layer:'boundary'});rules.push(evidenceRule);
const titles={
 'LA-A01':'Живая реакция','LA-A02':'Ответ по существу','LA-A03':'Полезное уточнение','LA-A04':'Продолжение без повторов',
 'LA-A05':'Связь реплики с постом','LA-A06':'Просмотр перед уточнением','LA-A07':'Проверка доступного факта','LA-A08':'Запчасти после смены модели',
 'LA-A09':'Цена на дату публикации','LA-A10':'Основание оставить вопрос открытым','LA-A11':'Альтернативный поиск при сбое','LA-A12':'Ответ об оцинковке',
 'LA-A13':'Обсуждение пожелания','LA-A16':'Расчёт через отдел продаж','LA-A17':'Обращение по официальному маршруту','LA-A18':'Номер договора для разбора',
 'LA-F01':'Без выдуманных сведений','LA-F02':'Сведения о действиях команды','LA-F03':'Соответствие версии автомобиля','LA-F04':'Подтверждение возможности поставки',
 'LA-F05':'Границы доступности запчастей','LA-F06':'Проверка названной цены','LA-F07':'Границы защиты от коррозии','LA-F08':'Границы выводов из медиа',
 'LA-F09':'Исправление технической ошибки','LA-S01':'Естественный тон на равных','LA-S02':'Краткость с сохранением смысла','LA-S03':'Уместные эмодзи и юмор',
 'LA-O01':'Чистый публичный текст','LA-O02':'Проверенные контакты','LA-E02':'Проверка перед согласием или опровержением','LA-E04':'История по точному автору',
 'LA-E07':'Сведения клиента о заказе','LA-E13':'Оговорка без отписки','LA-E14':'Уважение чужого выбора','LA-E17':'Явное расхождение чисел',
 'LA-E18':'Подтверждённые условия измерения','LA-E19':'Приоритет источников характеристик','LA-E20':'Проверка раздела источника','LA-E21':'Колонки и сноски таблицы',
 'LA-E22':'Полное содержание сохраняемого факта'
};
for(const r of rules){assert(titles[r.id],r.id);r.title=titles[r.id];}
assert.equal(new Set(rules.map(r=>r.title)).size,rules.length);
const legacyEvidence=[{id:'LEGACY-E22-FIELDS',sourceClauseIds:['FC08'],reason:'Прежние facts.claim/source_url не совпадают с текущим evidence[{itemId,url,title,claim}]; содержательное требование сохранено в LA-E22.',evidenceRefs:['mvp/adapters/assistant.mjs:697','mvp/adapters/assistant.mjs:747']}];
const archiveReasons={
 'LA-E08':'Пересмотр реакции принадлежит текущему second-pass протоколу, не политике бренда; он уже разрешает close→reply.',
 'LA-E09':'Сохранение фактов, оговорок и контактов обеспечивается LA-F01/LA-F03/LA-O02 и текущим review; старый контракт не становится отдельной политикой.',
 'LA-E10':'Текущий review уже запрещает менять уместный ответ ради стилистического разнообразия.',
 'LA-E11':'Неопределённая прежняя JSON-схема устарела; текущий review возвращает triage JSON + evidence.',
 'LA-E12':'Примеры сохранены только как иллюстративная трассировка/сценарии; сами примеры не поступают как новые факты или шаблоны.',
 'LA-E24':'needs_research — прежнее имя механизма; текущий движок сам выбирает исследовательский проход.',
 'LA-E25':'Необоснованное согласие уже запрещено активными LA-E02/LA-F01; отдельная команда прежнему second_pass не нужна.'
};
for(const r of contracts){r.disposition='archive_legacy_contract';r.archiveReason=archiveReasons[r.id];r.evidenceRefs=['mvp/adapters/assistant.mjs:52','mvp/adapters/assistant.mjs:728','mvp/adapters/assistant.mjs:733','mvp/adapters/assistant.mjs:747'];}
for(const r of rules){r.scope=scope;r.platformScope={mode:'all_in_account',routeSelection:'active_platform_only'};r.trust='imported_policy';r.layer=r.kind==='style_policy'?'how':r.kind==='action_policy'?'when_what':'boundary';r.grantsExecutionAuthority=false;}
const all=[...rules,...contracts,...constraints], byId=new Map(all.map(r=>[r.id,r]));
assert.equal(byId.size,all.length);
const clauses=[];
function addClause(sourceId,start,end,targetRuleIds,role='normative'){
 const {e,v}=selected.get(sourceId), fragment=v.text.slice(start,end),id=`${sourceId}:${String(clauses.filter(c=>c.sourceId===sourceId).length+1).padStart(3,'0')}`;
 const c={id,sourceId,entryId:e.id,versionId:v.id,startUtf16:start,endUtf16Exclusive:end,startByte:Buffer.byteLength(v.text.slice(0,start)),endByte:Buffer.byteLength(v.text.slice(0,end)),textSha256Utf8:hash(fragment),targetRuleIds:[...new Set(targetRuleIds.map(canon))],role};
 assert(c.targetRuleIds.length&&c.targetRuleIds.every(id=>byId.has(id)),id);clauses.push(c);return c;
}
for(let si=0;si<sourceOrder.length;si++){
 const sourceId=sourceOrder[si][0],text=selected.get(sourceId).v.text;let start=0,q=0,spans=[];
 const append=end=>{while(start<end&&/\s/.test(text[start]))start++;while(end>start&&/\s/.test(text[end-1]))end--;if(end>start)spans.push([start,end]);};
 for(let i=0;i<text.length;i++){if(text[i]==='«')q++;if(text[i]==='»')q--;if(q===0&&/[.!?]/.test(text[i])&&(i+1===text.length||/\s/.test(text[i+1]))){append(i+1);start=i+1;}}
 append(text.length);assert.equal(spans.length,core.sourceClauseTargets[si].length,sourceId);
 spans.forEach(([s,e],i)=>addClause(sourceId,s,e,[...core.sourceClauseTargets[si][i],...(sourceId==='G03'&&i===1?['LA-A16']:[])]));
}
const editorialMap=new Map(ed.suggestedMergeMap.map(m=>[m.sourceClauseId,m.targetRuleIds.map(canon)]));
const editorialClauseMap=ed.clauses.map(c=>({id:c.id,ruleIds:editorialMap.get(c.id),sourceRefs:c.sourceRefs,role:c.when.startsWith('Иллюстратив')?'illustrative':'normative'}));
for(const s of ed.sources){
 const text=selected.get(s.symbol).v.text;
 for(const ref of [...ed.clauses,...ed.examples].flatMap(x=>x.sourceRefs).filter(r=>r.sourceSymbol===s.symbol))assert.equal(hash(text.slice(ref.startUtf16,ref.endUtf16Exclusive)),ref.fragmentSha256Utf8);
 let start=0;for(const line of text.split('\n')){
  const end=start+line.length;
  if(line.trim()){
   const ids=ed.clauses.filter(c=>c.sourceRefs.some(r=>r.sourceSymbol===s.symbol&&r.startUtf16<end&&r.endUtf16Exclusive>start)).flatMap(c=>editorialMap.get(c.id));
   const ex=ed.examples.filter(c=>c.sourceRefs.some(r=>r.sourceSymbol===s.symbol&&r.startUtf16<end&&r.endUtf16Exclusive>start));
   for(const e of ex)ids.push('LA-E12',...e.linkedClauses.flatMap(id=>editorialMap.get(id)??[]));
   addClause(s.symbol,start,end,ids,ex.length?'normative_or_illustrative_line':'normative_line');
  }start=end+1;
 }
}
for(const c of constraints){const sourceId=constraintDefs.find(x=>x.id===c.id).sourceId;const row=addClause(sourceId,0,selected.get(sourceId).v.text.length,[c.id],'literal_constraint');row.id=sourceId+':all';}
for(const r of all){r.sourceClauseIds=clauses.filter(c=>c.targetRuleIds.includes(r.id)).map(c=>c.id);assert(r.sourceClauseIds.length,r.id);r.text=`Когда: ${r.when}\n${r.layer==='how'?'Как':'Что'}: ${r.action}${r.exceptions.length?'\nОговорки: '+r.exceptions.join(' '):''}${r.values?'\nБуквальные значения: '+JSON.stringify(r.values):''}`;}
const ambiguities=[
 {id:'AMB-CTA',status:'resolved_by_owner',ruleIds:['LA-A09','LA-A16'],ownerDecisionId:ctaDecision.id,sourceClauseIds:['G03:002','G18:001','L05:001',...clauses.filter(c=>c.sourceId==='REPLY_EDITING'&&c.targetRuleIds.some(id=>['LA-A09','LA-A16'].includes(id))).map(c=>c.id)],alternatives:[{condition:'Без менеджера невозможно продолжить.',action:'Только тогда CTA (прежний общий источник).'}, {condition:'Актуальная цена, наличие, подбор или расчёт.',action:'Направить по маршруту площадки; подтверждённый ответ CTA не требует (LikeAvto).'}, {condition:'Старый ролик и изменение цены.',action:'Общий источник предлагает актуальный расчёт; редакторский допускает конкретный контакт, если полезно.'}],requiredDecision:'Владелец разрешил направление в продажи при вопросе/неясности о стоимости и расчёте конкретных вариантов. Это явное уточнение, не молчаливый выбор старого приоритета.'},
 {id:'AMB-REACTION',status:'resolved_by_owner',ruleIds:['LA-A01'],ownerDecisionId:ownerDecision.id,alternatives:[{condition:'Личное наблюдение.',action:'Редакторское руководство: кратко отреагировать.'},{condition:'Нет полезного естественного добавления.',action:'Общие правила: ответ необязателен; пустая формальность не нужна.'}],requiredDecision:'Владелец уточнил «Да, по уместности»: обязательного пустого ответа нет. Это разрешённое уточнение политики, не доказательство прежней семантической эквивалентности.'}
];
const relations=[
 {type:'exact_semantic_duplicate',sources:['L00:003','L03:001'],targetRuleIds:['LA-F01'],resolution:'Один запрет выдуманных фактов; две ссылки происхождения.'},
 {type:'merged_overlap',sources:['G00','L00','G01','G08','G17','REPLY_EDITING'],targetRuleIds:['LA-S01','LA-S02','LA-O01'],resolution:'Тон, объём и публичный формат разделены; HOW не подменяет выбор реакции.'},
 {type:'merged_overlap',sources:['G06','G14','REPLY_EDITING','FACT_CHECKING'],targetRuleIds:['LA-A05','LA-A06','LA-F03','LA-E18'],resolution:'Контекст, просмотр и применимость факта сохранены как разные условия.'},
 {type:'merged_overlap',sources:['G04','G16','FACT_CHECKING'],targetRuleIds:['LA-A10','LA-A13'],resolution:'Недостающий факт не автоматический hold; обязательное внутреннее решение остаётся открытым.'},
 {type:'refinement_not_duplicate',sources:['G03','L05','REPLY_EDITING'],targetRuleIds:['LA-A09','LA-F06'],resolution:'Возраст ролика и подтверждение новой суммы — разные проверки; CTA не разрешён.'},
 {type:'layer_separation',sources:['G17','FACT_CHECKING'],targetRuleIds:['LA-O01','LA-E22'],resolution:'Прямой URL сохраняется в доказательстве, исследовательские источники не переносятся в публичный ответ.'},
 {type:'compatible_defaults',sources:['G08','REPLY_EDITING'],targetRuleIds:['LA-S02'],resolution:'1–3 предложения и предпочтение 1–2 фраз сохранены как ориентиры с исключением для содержательного вопроса, не жёсткий лимит.'}
];
const catalog={schemaVersion:1,candidateId:'likeavto-rules-2026-09-23-v1',status:'pending_owner_review',account:'LikeAvto',observedAtUtc:new Date().toISOString(),trust:'imported_policy',provenanceOrigin:'communityhero.rule-normalization-candidate',grantsExecutionAuthority:false,activationBlockedBy:ambiguities.filter(a=>a.status==='unresolved').map(a=>a.id),ownerDecisions:decisionRefs,scope,sourceStatus:'31 active imported rules; original immutable versions remain in storage',sources,rules,constraints,contracts,legacyEvidence,ambiguities,relations};
const coverage={schemaVersion:1,candidateId:catalog.candidateId,offsetContract:'UTF-16 slices are JS code-unit offsets; UTF-8 byte ranges are end-exclusive, nonoverlapping and cover every non-whitespace source character.',clauses,editorialClauseMap,examples:ed.examples,relations,coverage:sources.map(s=>({sourceId:s.id,entryId:s.entryId,versionId:s.versionId,clauseCount:clauses.filter(c=>c.sourceId===s.id).length,disposition:s.disposition}))};
for(const [id,{v}]of selected){const spans=clauses.filter(c=>c.sourceId===id).sort((a,b)=>a.startUtf16-b.startUtf16);let end=0;for(const c of spans){assert(c.startUtf16>=end);assert(!v.text.slice(end,c.startUtf16).trim(),id);end=c.endUtf16Exclusive;}assert(!v.text.slice(end).trim(),id);}
write('rules.v1.json',catalog);write('coverage.v1.json',coverage);
const activeIds=new Set(rules.map(r=>r.id));
const contractIds=new Set(contracts.map(r=>r.id));
const planCoverage=clauses.map(c=>{
 if(c.sourceId.startsWith('C_'))return {clauseId:c.id,retainedEntryId:c.entryId};
 const ruleIds=c.targetRuleIds.filter(id=>activeIds.has(id)),archivedIds=c.targetRuleIds.filter(id=>contractIds.has(id));
 const oldEvidenceFields=c.sourceId==='FACT_CHECKING'&&c.targetRuleIds.includes('LA-E22');
 return {clauseId:c.id,...(ruleIds.length?{ruleIds}:{}),...(archivedIds.length||oldEvidenceFields?{archivedLegacy:{reason:[...archivedIds.map(id=>archiveReasons[id]),...(oldEvidenceFields?[legacyEvidence[0].reason]:[])].join(' '),sourceContractIds:[...archivedIds,...(oldEvidenceFields?['LEGACY-E22-FIELDS']:[])],evidenceRefs:['mvp/adapters/assistant.mjs:52','mvp/adapters/assistant.mjs:728','mvp/adapters/assistant.mjs:733','mvp/adapters/assistant.mjs:747']}}:{})};
});
const plan={schemaVersion:1,requestId:'likeavto-normalize-20260923-v1',candidateId:catalog.candidateId,account:'LikeAvto',sources:sources.map(({entryId,versionId,versionHash,sourceHash,textSha256Utf8,scope,disposition})=>({entryId,versionId,versionHash,sourceHash,textSha256Utf8,scope,disposition})),clauses:clauses.map(({id,entryId,versionId,startByte,endByte,textSha256Utf8})=>({id,entryId,versionId,startByte,endByte,textSha256Utf8})),rules:rules.map(r=>({id:r.id,title:r.title,text:r.text,category:r.kind,scope:r.scope,sourceClauseIds:r.sourceClauseIds,kind:'rule',trust:'imported_policy',...(r.ownerDecisionIds?{ownerDecisionIds:r.ownerDecisionIds}:{})})),coverage:planCoverage,unresolvedAmbiguityIds:catalog.activationBlockedBy,ownerDecisions:decisionRefs};
write('activation-plan.v1.json',plan);
// Pure production projection, no model/CLI invocation. Placeholder version hashes
// have the exact fixed 64-hex width of the installer's real hashes.
const projectVersions=vs=>{
 const materials=vs.map(v=>({id:v.sourceMaterialId,title:v.title,text:v.text,kind:v.kind,revision:v.sourceRevision??null,postKey:v.postKey??null,sourceUrl:v.sourceUrl??null,knowledgeEntryId:v.entryId,knowledgeVersionId:v.id,trust:v.trust,...(v.companyImport?{companyImport:v.companyImport,sourceAssertion:true}:{})}));
 const knowledgeManifest=vs.map(v=>({entryId:v.entryId,versionId:v.id,hash:v.hash,kind:v.kind,scope:v.scope,trust:v.trust}));
 return prepareAssistantRequest({account:'LikeAvto',purpose:'triage',items:[],materials,knowledgeManifest,knowledgePolicyVersion:1});
};
const retainedVersions=[...selected].filter(([id])=>id.startsWith('C_')).map(([,x])=>x.v);
const plannedVersions=rules.map(r=>{const sourceMaterialId='normalized-rule-'+hash(JSON.stringify(['LikeAvto',catalog.candidateId,r.id])),entryId='knowledge-'+hash(sourceMaterialId),versionHash='0'.repeat(64);return {id:'knowledge-version-'+versionHash,hash:versionHash,entryId,sourceMaterialId,title:r.title,text:r.text,kind:'rule',sourceRevision:1,postKey:'',sourceUrl:'',scope,trust:'imported_policy'};});
const beforeProjection=projectVersions([...selected.values()].map(x=>x.v)),afterProjection=projectVersions([...retainedVersions,...plannedVersions]);
const size=s=>({utf16Units:s.length,utf8Bytes:Buffer.byteLength(s)});
const rulePayload=p=>JSON.stringify({materials:p.payload.materials,knowledgeManifest:p.payload.knowledgeManifest});
const promptCost={definition:'Actual prepareAssistantRequest serializer and rule semantics helper; rule-only synthetic empty-item request. Includes material titles, IDs, revisions, kind/trust and knowledge manifest; excludes unchanged system instructions and real item/post/private context. Planned version hashes use fixed-width 64-hex placeholders: byte/character cost is exact for this planned shape, not a live prepared bundle and not token usage.',serializer:'mvp/adapters/assistant.mjs:prepareAssistantRequest',before:{materialCount:beforeProjection.payload.materials.length,rulePayload:size(rulePayload(beforeProjection)),request:size(beforeProjection.input)},after:{materialCount:afterProjection.payload.materials.length,rulePayload:size(rulePayload(afterProjection)),request:size(afterProjection.input)},modelCalls:0};
promptCost.rulePayloadDeltaUtf8=promptCost.after.rulePayload.utf8Bytes-promptCost.before.rulePayload.utf8Bytes;
promptCost.rulePayloadReductionPercentUtf8=Number((100*(1-promptCost.after.rulePayload.utf8Bytes/promptCost.before.rulePayload.utf8Bytes)).toFixed(2));
write('prompt-cost.v1.json',promptCost);
const scenarioCases=[...read('scenarios.v1.json').cases,...read('editorial-scenarios.json').cases].map(c=>({...c,ruleIds:[...new Set(c.ruleIds.map(canon))],...(c.excludedRuleIds?{excludedRuleIds:[...new Set(c.excludedRuleIds.map(canon))]}:{})}));
scenarioCases.find(c=>c.id==='EV-25').ruleIds.push('LA-E14');
scenarioCases.find(c=>c.id==='EV-01').expect[0]='Сначала найти уместную короткую тёплую реакцию; пропуск допустим, если добавление действительно лишнее или повторное.';
scenarioCases.find(c=>c.id==='EV-09').expect[1]='Предложить актуальный расчёт через продажи по маршруту площадки, сохранив полезное пояснение исторической цены.';
delete scenarioCases.find(c=>c.id==='EV-09').ambiguities;
scenarioCases.find(c=>c.id==='EV-E11').input.task='Сохранить результат проверки для повторного использования.';
scenarioCases.find(c=>c.id==='EV-E11').expect[0]='Сохранить версию, единицу, условия, ограничение и прямую ссылку в действующей схеме доказательства; не навязывать старые поля facts/source_url.';
scenarioCases.push(
 {id:'EV-32',input:{account:'LikeAvto',comment:'Вот это цвет, огонь!',history:'Реплика новая, повторных благодарностей бренда нет.'},ruleIds:['LA-A01','LA-S01','LA-S03'],ownerDecisionIds:[ownerDecision.id],expect:['Активно найти краткую уместную тёплую реакцию или шутку.'],reject:['Автоматически закрыть только потому, что нет вопросительного знака.','Обязательный механический вопрос или пустая формальность ради процента ответов.']},
 {id:'EV-33',input:{account:'LikeAvto',platform:'VK',comment:'Не понимаю, сколько выйдет этот вариант с доставкой.',context:'Известны версия и состав услуги; точного актуального расчёта нет.',routes:'Есть проверенный маршрут VK.'},ruleIds:['LA-A16','LA-F01','LA-O02'],ownerDecisionIds:[ctaDecision.id],expect:['Дать известную полезную информацию и направить в продажи за расчётом конкретного варианта.'],reject:['Требовать доказать, что без менеджера разговор вообще невозможен.','Придумать смету или направить в чужой аккаунт.']}
);
const scenarioSuite={schemaVersion:1,status:'not_model_executed',candidateId:catalog.candidateId,source:'Synthetic/source-guide illustrative cases only; no customer data',cases:scenarioCases,ruleCoverage:all.map(r=>({ruleId:r.id,scenarioIds:scenarioCases.filter(c=>c.ruleIds.includes(r.id)).map(c=>c.id)})),aliasMap:aliases};
write('scenario-suite.v1.json',scenarioSuite);
const modelText=[...rules,...constraints].map(r=>r.text).join('\n\n');
const originalUnits=sources.reduce((a,s)=>a+s.textLengthUtf16,0),originalBytes=sources.reduce((a,s)=>a+s.textBytesUtf8,0);
const metrics={schemaVersion:1,sourceEntries:sources.length,sourceVersionCount:sources.length,sourceClauseCount:clauses.length,editorialAtoms:ed.clauses.length,editorialExamples:ed.examples.length,normalizedRules:rules.length,retainedTypedConstraints:constraints.length,archivedLegacyContracts:contracts.length,archivedLegacyFieldConventions:1,scenarioCount:scenarioCases.length,original:{utf16Units:originalUnits,utf8Bytes:originalBytes},normalizedCandidateText:{definition:'Concatenated explicit WHEN/action/exception text of active rules + literal constraints; excludes archived contracts, IDs, provenance, category labels and decision explanations. Actual serialized prompt cost is separate in prompt-cost.v1.json.',utf16Units:modelText.length,utf8Bytes:Buffer.byteLength(modelText)},reductionPercentUtf16:Number((100*(1-modelText.length/originalUnits)).toFixed(2)),tokenCount:'not measured',semanticEquivalence:'normalization plus two deliberate owner clarifications; review pending; structural coverage is not semantic proof',activationPerformed:false};
write('metrics.v1.json',metrics);
const lines=['# LikeAvto: нормализованный каталог, кандидат v1','','Статус: не активирован. Доверие остаётся `imported_policy`, фактов с повышенным доверием и права выполнения операций не добавлено. Аккаунт: LikeAvto; все площадки этого аккаунта, маршрут выбирается для фактической площадки. Чужие аккаунты вне области.','','31 исходная активная версия сохранена в БД. Здесь находятся нормализованный текст и ссылки на точные версии/хеши, без выгрузки клиентских кейсов.','','## До активации','','Оба спорных условия уточнены владельцем; точные ответы и происхождение сохранены в owner-decision-*.v1.json. План ждёт отдельной проверки и атомарного применения владельцем интеграции. Изменения реакции и CTA сознательно меняют политику, а не объявляются семантически тождественными старым формулировкам.'];
for(const a of ambiguities){lines.push('',`**${a.id}.** ${a.requiredDecision}`,'','Прежние формулировки для сверки, не действующая альтернативная инструкция:','',...a.alternatives.map(x=>`- Когда ${x.condition} ${x.action}`));}
for(const [heading,items]of [['Действия: когда и что',rules.filter(r=>r.layer==='when_what')],['Достоверность, идентичность и публичные ограничения',rules.filter(r=>r.layer==='boundary')],['Стиль: как формулировать',rules.filter(r=>r.layer==='how')],['Буквальные ограничения: исходные записи сохраняются',constraints],['Архивные контракты: не активируются и не подаются модели',contracts]]){lines.push('',`## ${heading}`);for(const r of items){lines.push('',`### ${r.id} · ${r.title??r.category}`,'',r.text.replaceAll('\n','\n\n'));if(r.operator)lines.push('',`Оператор: \`${JSON.stringify(r.operator)}\``);if(r.unresolvedReference)lines.push('',r.unresolvedReference);if(r.archiveReason)lines.push('',`Архив: ${r.archiveReason}`);if(r.ownerDecisionIds)lines.push('',`Уточнение владельца: ${r.ownerDecisionIds.join(', ')}.`);}}
lines.push('','## Проверка и использование','','`rules.v1.json` — каталог с типами, WHEN/WHAT/HOW, областью, оговорками и происхождением. `coverage.v1.json` — полная карта 31 старой версии и её клауз, byte/UTF-16 диапазоны и SHA-256. `activation-plan.v1.json` — отдельный вход будущей атомарной установки; не инструкция выполнить её. Три типизированных массива сохраняются как исходные записи с узнаваемым происхождением, не маскируются под новый импорт.','','Примеры остаются иллюстрациями, а не фактами об автомобилях и не готовыми шаблонами. Межпостовые клиентские кейсы и исходный частный каталог не сохранены.','','Итоговые 48 сценариев в `scenario-suite.v1.json` учитывают оба уточнения владельца и задают ожидаемую семантику. Отдельные authored/editorial файлы — промежуточный анализ, не вход активации. Модель не запускалась; проверка структуры и полного покрытия не доказывает эквивалентность смысла.','','Исходник: '+originalUnits+' UTF-16 единиц / '+originalBytes+' UTF-8 байт. Текст активных правил с WHEN/действием/оговорками и неизменёнными значениями массивов: '+modelText.length+' UTF-16 единиц / '+Buffer.byteLength(modelText)+' UTF-8 байт ('+metrics.reductionPercentUtf16+'% короче по UTF-16). Метаданные трассировки в это сравнение не входят; токены не измерялись.','','Локальная проверка: `node project/rules-normalization-candidate-2026-09-23/verify.mjs`. Повторная сверка голов читает только локальный каталог: добавить `--check-heads`. Никакого применения к БД эти команды не выполняют.');
lines.push('', 'Реальная проекция правил в JSON через prepareAssistantRequest (заголовки, ID, материалы и manifest): '+promptCost.before.rulePayload.utf8Bytes+' → '+promptCost.after.rulePayload.utf8Bytes+' UTF-8 байт. Более короткий текст не гарантирует меньший вход: атомарные записи добавляют служебные поля. Полный метод и ограничения в prompt-cost.v1.json.','', 'Итог плана: заменить 28 прежних текстовых записей на '+rules.length+' атомарных правил; сохранить три типизированных ограничения. Семь прежних контрактов и старые имена полей факта архивируются с точным clause disposition, не становятся активной политикой. Исходные версии не изменяются.');
write('rules.v1.ru.md',lines.join('\n')+'\n');
console.log(JSON.stringify({sources:sources.length,rules:rules.length,constraints:constraints.length,contracts:contracts.length,clauses:clauses.length,originalUnits,normalizedUnits:modelText.length,unresolved:catalog.activationBlockedBy}));
