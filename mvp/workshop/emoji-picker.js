const CATEGORIES = [
  ['all', 'Все'], ['faces', 'Эмоции'], ['hands', 'Жесты'],
  ['hearts', 'Любовь'], ['animals', 'Природа'], ['food', 'Еда'],
  ['travel', 'Поездки'], ['things', 'Разное'], ['symbols', 'Знаки'],
];
const EMOJI = [
  ['😀','Улыбка радость','faces'], ['😃','Счастливая улыбка','faces'],
  ['😄','Смеюсь радость','faces'], ['😁','Довольная улыбка','faces'],
  ['😆','Смех','faces'], ['😅','Улыбка с каплей неловко','faces'],
  ['😂','Слёзы радости смех','faces'], ['🤣','Очень смешно хохот','faces'],
  ['😊','Тёплая улыбка спасибо','faces'], ['🙂','Лёгкая улыбка','faces'],
  ['😉','Подмигивание','faces'], ['🙃','Перевёрнутая улыбка ирония','faces'],
  ['😍','Влюблённость','faces'], ['🥰','Нежность любовь','faces'],
  ['😘','Воздушный поцелуй','faces'], ['😎','Круто очки','faces'],
  ['🤩','Восторг','faces'], ['🥳','Праздничное настроение','faces'],
  ['🤔','Думаю вопрос','faces'], ['🧐','Любопытство изучаю','faces'],
  ['🤨','Сомнение','faces'], ['😐','Без слов','faces'],
  ['😶','Молчание','faces'], ['🫣','Подглядываю неловко','faces'],
  ['😳','Смущение удивление','faces'], ['😮','Удивление','faces'],
  ['🥹','Тронут до слёз','faces'],
  ['🥲','Улыбка сквозь слёзы','faces'], ['😔','Грусть','faces'],
  ['😢','Слеза печаль','faces'], ['😭','Плачу','faces'],
  ['😤','Недовольство','faces'], ['😬','Неловкость','faces'],
  ['🤗','Обнимаю','faces'], ['🤝','Рукопожатие договорились','hands'],
  ['👍','Палец вверх хорошо','hands'], ['👎','Палец вниз','hands'],
  ['👌','Всё отлично окей','hands'], ['✌️','Победа мир','hands'],
  ['🤞','Удачи','hands'], ['🤟','Люблю','hands'],
  ['🤘','Рок круто','hands'], ['🤙','На связи','hands'],
  ['👋','Привет пока','hands'], ['🖐️','Ладонь привет','hands'],
  ['👏','Аплодисменты','hands'], ['🙌','Ура','hands'],
  ['👐','Открытые ладони','hands'], ['🙏','Спасибо пожалуйста','hands'],
  ['💪','Сила молодец','hands'], ['🫶','Сердце руками','hands'],
  ['❤️','Красное сердце любовь','hearts'], ['🧡','Оранжевое сердце','hearts'],
  ['💛','Жёлтое сердце','hearts'], ['💚','Зелёное сердце','hearts'],
  ['💙','Синее сердце','hearts'], ['💜','Фиолетовое сердце','hearts'],
  ['🖤','Чёрное сердце','hearts'], ['🤍','Белое сердце','hearts'],
  ['💕','Два сердца','hearts'], ['💖','Сверкающее сердце','hearts'],
  ['💯','Сто процентов','things'], ['🔥','Огонь','things'],
  ['✨','Искры блеск','things'], ['⭐','Звезда','things'],
  ['🎉','Праздник поздравляю','things'], ['🎊','Конфетти','things'],
  ['🎁','Подарок','things'], ['🏆','Кубок победа','things'],
  ['🚗','Автомобиль машина','travel'], ['🚙','Внедорожник джип','travel'],
  ['🏎️','Гоночная машина','travel'], ['🛻','Пикап','travel'],
  ['🚀','Ракета вперёд','travel'], ['🔧','Ремонт инструмент','things'],
  ['⚡','Молния быстро','things'], ['💡','Идея','things'],
  ['📍','Место адрес','things'], ['📸','Фото камера','things'],
  ['☕','Кофе','food'], ['🌞','Солнце','animals'],
  ['🌷','Цветок','animals'], ['🍀','Удача клевер','animals'],
  ['✅','Готово верно','symbols'], ['❓','Вопрос','symbols'],
  ['👀','Смотрю глаза','things'], ['💬','Обсуждение комментарий','things'],
  ...[
    ['😋','Вкусно облизываюсь'], ['😛','Показываю язык'],
    ['😜','Подмигиваю язык шутка'], ['🤪','Дурачусь безумие'],
    ['😝','Дразнюсь язык'], ['🤑','Деньги богатство'],
    ['🤭','Прикрываю рот смешок'], ['🫢','Ой удивление ладонь у рта'],
    ['🫡','Отдаю честь принято'], ['🤫','Тихо секрет'],
    ['🤐','Рот на замке секрет'], ['😑','Без эмоций'],
    ['😏','Усмешка'], ['😒','Не впечатляет'],
    ['🙄','Закатываю глаза'], ['😌','Облегчение спокойствие'],
    ['😴','Сплю сон'], ['🥱','Зеваю усталость'],
    ['😪','Сонный'], ['🤤','Слюнки хочу'],
    ['😷','Медицинская маска'], ['🤒','Болею температура'],
    ['🤕','Повязка травма'], ['🤢','Тошнит'],
    ['🤧','Чихаю простуда'], ['🥵','Жарко'],
    ['🥶','Холодно замёрз'], ['🥴','Растерянность ошалел'],
    ['😵','Головокружение'], ['🤯','Взрыв мозга потрясение'],
    ['🥺','Прошу умоляю'], ['😕','Непонимание'],
    ['🫤','Неуверенность'], ['😟','Тревога'],
    ['🙁','Небольшая грусть'], ['☹️','Печаль'],
    ['😲','Очень удивлён'], ['😰','Переживаю тревога'],
    ['😥','Разочарование'], ['😓','Усталость пот'],
    ['😩','Измучен'], ['😫','Устал до предела'],
    ['😠','Злюсь'], ['😡','Сильная злость'],
    ['🤠','Ковбой'], ['🥸','Маскировка'],
    ['🤓','Умник ботан'], ['😇','Ангел невиновен'],
    ['🤖','Робот искусственный интеллект'], ['👻','Привидение'],
    ['👽','Пришелец'], ['🤡','Клоун'],
    ['😺','Кот улыбается'], ['😸','Кот радуется'],
    ['😹','Кот смеётся до слёз'], ['😻','Влюблённый кот'],
    ['😼','Кот усмехается'], ['😽','Кот целует'],
    ['🙀','Кот в шоке'], ['😿','Кот плачет'],
    ['😾','Недовольный кот'], ['🙈','Ничего не вижу обезьяна'],
    ['🙉','Ничего не слышу обезьяна'], ['🙊','Ничего не скажу обезьяна'],
  ].map(([emoji,label]) => [emoji,label,'faces']),
  ...[
    ['🤌','Щепотка пальцы'], ['🤏','Чуть-чуть'],
    ['🫰','Сердечко пальцами'], ['🤜','Кулак вправо'],
    ['🤛','Кулак влево'], ['👊','Кулак приветствие'],
    ['✊','Поднятый кулак'], ['🫳','Ладонь вниз'],
    ['🫴','Ладонь вверх предлагаю'], ['🫵','Ты указание'],
    ['👈','Указание влево'], ['👉','Указание вправо'],
    ['👆','Указание вверх'], ['👇','Указание вниз'],
    ['☝️','Внимание один момент'], ['✍️','Пишу'],
    ['🤳','Селфи'], ['💅','Маникюр'],
    ['🦾','Механическая рука сила'], ['🖖','Приветствие вулканцев'],
  ].map(([emoji,label]) => [emoji,label,'hands']),
  ...[
    ['🩷','Розовое сердце'], ['🩵','Голубое сердце'],
    ['🩶','Серое сердце'], ['🤎','Коричневое сердце'],
    ['💔','Разбитое сердце'], ['❤️‍🔥','Пылающее сердце'],
    ['❤️‍🩹','Заживающее сердце'], ['❣️','Сердце восклицание'],
    ['💞','Вращающиеся сердца'], ['💓','Бьющееся сердце'],
    ['💗','Растущее сердце'], ['💘','Сердце со стрелой'],
    ['💝','Сердце подарок'], ['💟','Украшение сердце'],
    ['💌','Любовное письмо'], ['💋','Поцелуй губы'],
  ].map(([emoji,label]) => [emoji,label,'hearts']),
  ...[
    ['🐶','Собака пёс'], ['🐱','Кошка кот'],
    ['🐭','Мышка'], ['🐹','Хомяк'],
    ['🐰','Кролик заяц'], ['🦊','Лиса'],
    ['🐻','Медведь'], ['🐼','Панда'],
    ['🐨','Коала'], ['🐯','Тигр'],
    ['🦁','Лев'], ['🐮','Корова'],
    ['🐷','Поросёнок'], ['🐸','Лягушка'],
    ['🐵','Обезьяна'], ['🐔','Курица'],
    ['🐧','Пингвин'], ['🐦','Птица'],
    ['🐤','Цыплёнок'], ['🦆','Утка'],
    ['🦉','Сова'], ['🦅','Орёл'],
    ['🦋','Бабочка'], ['🐝','Пчела'],
    ['🐞','Божья коровка'], ['🐌','Улитка медленно'],
    ['🐢','Черепаха'], ['🐬','Дельфин'],
    ['🐳','Кит'], ['🐟','Рыба'],
    ['🐙','Осьминог'], ['🦄','Единорог'],
    ['🐎','Лошадь'], ['🦌','Олень'],
    ['🐕','Собака'], ['🐈','Кот кошка'],
    ['🌹','Роза'], ['🌸','Сакура цветок'],
    ['🌻','Подсолнух'], ['🌼','Ромашка'],
    ['💐','Букет'], ['🌺','Гибискус'],
    ['🌱','Росток'], ['🌿','Зелень листья'],
    ['🌳','Дерево'], ['🌲','Ель лес'],
    ['🌴','Пальма'], ['🍁','Кленовый лист осень'],
    ['🍂','Опавшие листья'], ['🌈','Радуга'],
    ['☀️','Ясное солнце'], ['🌤️','Солнце за облаком'],
    ['☁️','Облако'], ['🌧️','Дождь'],
    ['⛈️','Гроза'], ['❄️','Снежинка зима'],
    ['☃️','Снеговик'], ['🌊','Море волна'],
    ['🌙','Луна ночь'], ['🌍','Земля мир'],
  ].map(([emoji,label]) => [emoji,label,'animals']),
  ...[
    ['🍎','Яблоко'], ['🍐','Груша'],
    ['🍊','Апельсин мандарин'], ['🍋','Лимон'],
    ['🍌','Банан'], ['🍉','Арбуз'],
    ['🍇','Виноград'], ['🍓','Клубника'],
    ['🍒','Вишня'], ['🍑','Персик'],
    ['🥭','Манго'], ['🍍','Ананас'],
    ['🥝','Киви'], ['🥑','Авокадо'],
    ['🥕','Морковь'], ['🌶️','Острый перец'],
    ['🥒','Огурец'], ['🍅','Помидор'],
    ['🥐','Круассан'], ['🍞','Хлеб'],
    ['🥖','Багет'], ['🧀','Сыр'],
    ['🍕','Пицца'], ['🍔','Бургер'],
    ['🍟','Картофель фри'], ['🌭','Хот-дог'],
    ['🥪','Сэндвич'], ['🥗','Салат'],
    ['🍜','Лапша суп'], ['🍣','Суши'],
    ['🍦','Мороженое'], ['🍩','Пончик'],
    ['🍪','Печенье'], ['🎂','Торт день рождения'],
    ['🍰','Кусочек торта'], ['🍫','Шоколад'],
    ['🍬','Конфета'], ['🍭','Леденец'],
    ['🍯','Мёд'], ['🫖','Чайник'],
    ['🍵','Чай'], ['🥤','Напиток'],
    ['🧃','Сок'], ['🥛','Молоко'],
  ].map(([emoji,label]) => [emoji,label,'food']),
  ...[
    ['🚕','Такси'], ['🚌','Автобус'],
    ['🚐','Микроавтобус'], ['🚚','Грузовик доставка'],
    ['🚛','Фура'], ['🚜','Трактор'],
    ['🏍️','Мотоцикл'], ['🛵','Мотороллер'],
    ['🚲','Велосипед'], ['🛴','Самокат'],
    ['🚆','Поезд'], ['🚇','Метро'],
    ['🚄','Скоростной поезд'], ['✈️','Самолёт перелёт'],
    ['🚁','Вертолёт'], ['⛵','Парусник'],
    ['🚢','Корабль'], ['🛞','Колесо шина'],
    ['⛽','Заправка топливо'], ['🚦','Светофор'],
    ['🚧','Дорожные работы'], ['🛣️','Дорога трасса'],
    ['🏠','Дом'], ['🏡','Дом с садом'],
    ['🏢','Офис здание'], ['🏭','Завод производство'],
    ['🏖️','Пляж отпуск'], ['🏕️','Кемпинг палатка'],
    ['🏔️','Горы'], ['🏙️','Город'],
    ['🌅','Рассвет'], ['🌇','Закат'],
    ['🗺️','Карта путешествие'], ['🧭','Компас направление'],
    ['🧳','Чемодан поездка'], ['🛂','Паспортный контроль'],
  ].map(([emoji,label]) => [emoji,label,'travel']),
  ...[
    ['📱','Телефон смартфон'], ['💻','Ноутбук компьютер'],
    ['⌚','Часы'], ['🎧','Наушники'],
    ['🎵','Музыка нота'], ['🎶','Мелодия ноты'],
    ['🎬','Видео кино'], ['📹','Видеокамера'],
    ['📷','Фотоаппарат'], ['🎮','Игры геймпад'],
    ['⚽','Футбол мяч'], ['🏀','Баскетбол'],
    ['🎯','Цель мишень'], ['🏅','Медаль'],
    ['🛠️','Инструменты ремонт'], ['⚙️','Настройки шестерёнка'],
    ['🔩','Болт гайка'], ['🔑','Ключ'],
    ['🔒','Закрыто замок'], ['🔓','Открыто замок'],
    ['📦','Посылка коробка'], ['📚','Книги знания'],
    ['📝','Заметка запись'], ['📅','Календарь дата'],
    ['📌','Закрепить кнопка'], ['🔔','Уведомление колокольчик'],
    ['🔍','Поиск'], ['💰','Деньги стоимость'],
    ['💳','Банковская карта оплата'], ['📈','Рост график'],
    ['📉','Падение график'], ['💎','Бриллиант ценность'],
  ].map(([emoji,label]) => [emoji,label,'things']),
  ...[
    ['✔️','Галочка подтверждено'], ['❌','Крестик нет'],
    ['⚠️','Внимание предупреждение'], ['⛔','Стоп запрещено'],
    ['🚫','Запрет'], ['ℹ️','Информация'],
    ['❗','Восклицательный знак важно'], ['‼️','Очень важно'],
    ['⁉️','Удивлённый вопрос'], ['💤','Сон отдых'],
    ['♻️','Переработка повторно'], ['🔄','Обновление повтор'],
    ['➕','Плюс добавить'], ['➖','Минус убрать'],
    ['➡️','Стрелка вправо'], ['⬅️','Стрелка влево'],
    ['⬆️','Стрелка вверх'], ['⬇️','Стрелка вниз'],
    ['🔴','Красный круг'], ['🟠','Оранжевый круг'],
    ['🟡','Жёлтый круг'], ['🟢','Зелёный круг'],
    ['🔵','Синий круг'], ['🟣','Фиолетовый круг'],
    ['⚪','Белый круг'], ['⚫','Чёрный круг'],
    ['🔶','Оранжевый ромб'], ['🔷','Синий ромб'],
    ['🏁','Финиш'], ['🚩','Красный флаг'],
    ['⏳','Ожидание песочные часы'], ['⏰','Будильник время'],
  ].map(([emoji,label]) => [emoji,label,'symbols']),
].filter(([emoji], index, all) => all.findIndex(entry => entry[0] === emoji) === index);

const bindings = new WeakMap();
let picker;
let active;

export function emojiButton(id) {
  const safeId = String(id || '').replace(/[&<>"']/g, character => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[character]));
  return `<button type="button" id="${safeId}" class="icon-button emoji-toggle" aria-label="Добавить смайлик" title="Добавить смайлик"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M8 14.5s1.3 2 4 2 4-2 4-2"/><path d="M8.5 8.5h.01M15.5 8.5h.01" stroke-width="2.5"/></svg></button>`;
}

function createElement(tag, className, text) {
  const element = document.createElement(tag);
  element.className = className;
  if (text) element.textContent = text;
  return element;
}

function closePicker() {
  active?.button.setAttribute('aria-expanded', 'false');
  active = undefined;
  picker?.panel.hidePopover();
}

function clampSelection(owner) {
  owner.start = Math.max(0, Math.min(owner.start || 0, owner.textarea.value.length));
  owner.end = Math.max(owner.start, Math.min(owner.end || 0, owner.textarea.value.length));
}

// Background refresh replaces composers. Keep a picker attached to its logical
// editor when its text has not changed, even if another composer binds first.
function reconcileActive() {
  if (!active) return false;
  const button = active.button.isConnected ? active.button : document.getElementById(active.buttonId);
  const textarea = active.textarea.isConnected ? active.textarea : document.getElementById(active.textareaId);
  if (!button || !textarea) return false;
  if (textarea.value !== active.value || textarea.disabled || textarea.readOnly) {
    closePicker();
    return false;
  }
  active.button = button;
  active.textarea = textarea;
  clampSelection(active);
  button.setAttribute('aria-expanded', 'true');
  return true;
}

function positionPicker() {
  if (!active || !picker) return;
  if (!reconcileActive()) return;
  const anchor = active.button.getBoundingClientRect();
  const {width, height} = picker.panel.getBoundingClientRect();
  const viewport = window.visualViewport;
  const left = viewport?.offsetLeft || 0;
  const top = viewport?.offsetTop || 0;
  const right = left + (viewport?.width || window.innerWidth);
  const bottom = top + (viewport?.height || window.innerHeight);
  picker.panel.style.left = `${Math.max(left + 8, Math.min(anchor.left, right - width - 8))}px`;
  const above = anchor.top - height - 8;
  picker.panel.style.top = `${Math.max(top + 8, Math.min(above >= top + 8 ? above : anchor.bottom + 8, bottom - height - 8))}px`;
}

function ensurePicker() {
  if (picker) return picker;
  const panel = createElement('div', 'emoji-picker');
  panel.id = 'composer-emoji-picker';
  panel.setAttribute('popover', 'auto');
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-label', 'Выбрать смайлик');
  const header = createElement('div', 'emoji-picker-header');
  header.append(createElement('strong', '', 'Смайлики'));
  const close = createElement('button', 'emoji-picker-close', '×');
  close.type = 'button';
  close.setAttribute('aria-label', 'Закрыть смайлики');
  header.append(close);
  const search = createElement('input', 'emoji-picker-search');
  search.type = 'search';
  search.placeholder = 'Найти смайлик';
  search.setAttribute('aria-label', 'Найти смайлик');
  const categories = createElement('div', 'emoji-picker-categories');
  categories.setAttribute('aria-label', 'Категории смайликов');
  const grid = createElement('div', 'emoji-picker-grid');
  grid.setAttribute('role', 'group');
  grid.setAttribute('aria-label', 'Смайлики');
  const empty = createElement('p', 'emoji-picker-empty', 'Ничего не найдено');
  empty.setAttribute('role', 'status');
  panel.append(header, search, categories, grid, empty);
  document.body.append(panel);
  picker = {panel, search, category:'all', grid, categories, empty};

  for (const [id, label] of CATEGORIES) {
    const button = createElement('button', 'emoji-picker-category', label);
    button.type = 'button';
    button.dataset.category = id;
    button.addEventListener('click', () => {picker.category = id; renderEmoji(); positionPicker();});
    categories.append(button);
  }
  close.addEventListener('click', () => {
    const owner = active;
    panel.hidePopover();
    owner?.button.focus({preventScroll:true});
  });
  search.addEventListener('input', () => {renderEmoji(); positionPicker();});
  panel.addEventListener('toggle', event => {
    if (event.newState !== 'closed') return;
    active?.button.setAttribute('aria-expanded', 'false');
    active = undefined;
  });
  grid.addEventListener('click', event => {
    const emoji = event.target.closest('button[data-emoji]')?.dataset.emoji;
    if (!emoji || !reconcileActive()) return;
    const owner = active;
    if (!owner.textarea.isConnected || owner.textarea.disabled || owner.textarea.readOnly) {
      panel.hidePopover();
      return;
    }
    const {textarea, start, end} = owner;
    textarea.focus({preventScroll:true});
    textarea.setRangeText(emoji, start, end, 'end');
    owner.start = owner.end = textarea.selectionStart;
    owner.value = textarea.value;
    closePicker();
    textarea.dispatchEvent(new Event('input', {bubbles:true}));
  });
  grid.addEventListener('keydown', event => {
    const buttons = Array.from(grid.querySelectorAll('button'));
    const index = buttons.indexOf(document.activeElement);
    const step = {ArrowRight:1, ArrowLeft:-1, ArrowDown:8, ArrowUp:-8}[event.key];
    if (index < 0 || !step) return;
    event.preventDefault();
    buttons[Math.max(0, Math.min(buttons.length - 1, index + step))]?.focus();
  });
  window.addEventListener('resize', positionPicker);
  window.addEventListener('scroll', positionPicker, {capture:true, passive:true});
  window.visualViewport?.addEventListener('resize', positionPicker);
  window.visualViewport?.addEventListener('scroll', positionPicker);
  return picker;
}

function renderEmoji() {
  const query = picker.search.value.toLocaleLowerCase('ru').replaceAll('ё', 'е').trim();
  const matches = EMOJI.filter(([emoji, label, category]) =>
    (picker.category === 'all' || picker.category === category)
    && `${emoji} ${label}`.toLocaleLowerCase('ru').replaceAll('ё', 'е').includes(query));
  picker.grid.replaceChildren(...matches.map(([emoji, label]) => {
    const button = createElement('button', 'emoji-picker-emoji', emoji);
    button.type = 'button';
    button.dataset.emoji = emoji;
    button.setAttribute('aria-label', label);
    button.title = label;
    return button;
  }));
  picker.empty.hidden = matches.length !== 0;
  for (const button of picker.categories.children) {
    button.setAttribute('aria-pressed', String(button.dataset.category === picker.category));
  }
}

/** Bind each composer after render. Rebinding is safe; returns optional cleanup. */
export function bindEmojiPicker(button, textarea) {
  if (!button || !textarea) return () => {};
  const previous = bindings.get(button);
  reconcileActive();
  const preserve = active && active.button === button && active.textarea === textarea;
  previous?.dispose(true);
  const controller = new AbortController();
  const options = {signal:controller.signal};
  const owner = preserve ? active : {button, textarea, buttonId:button.id, textareaId:textarea.id,
    value:textarea.value, start:textarea.selectionStart, end:textarea.selectionEnd};
  let dismissOnClick = false;
  const rememberSelection = () => {
    if (active === owner && picker?.panel.matches(':popover-open') && document.activeElement !== textarea) return;
    owner.start = textarea.selectionStart;
    owner.end = textarea.selectionEnd;
    owner.value = textarea.value;
    clampSelection(owner);
  };
  button.type = 'button';
  button.setAttribute('aria-haspopup', 'dialog');
  button.setAttribute('aria-expanded', String(!!preserve));
  button.setAttribute('aria-controls', 'composer-emoji-picker');
  if (!button.hasAttribute('aria-label')) button.setAttribute('aria-label', 'Добавить смайлик');
  button.addEventListener('pointerdown', () => {
    rememberSelection();
    // Native light-dismiss may happen before click reaches the trigger.
    dismissOnClick = active === owner && picker?.panel.matches(':popover-open');
  }, options);
  for (const event of ['select', 'keyup', 'pointerup', 'blur', 'input']) {
    textarea.addEventListener(event, rememberSelection, options);
  }
  button.addEventListener('click', () => {
    if (textarea.disabled || textarea.readOnly) return;
    const current = ensurePicker();
    if (dismissOnClick || (active === owner && current.panel.matches(':popover-open'))) {
      dismissOnClick = false;
      current.panel.hidePopover();
      return;
    }
    active?.button.setAttribute('aria-expanded', 'false');
    active = owner;
    owner.value = textarea.value;
    clampSelection(owner);
    current.category = 'all';
    current.search.value = '';
    renderEmoji();
    button.setAttribute('aria-expanded', 'true');
    current.panel.showPopover();
    positionPicker();
    current.search.focus({preventScroll:true});
  }, options);
  const dispose = (preserveOpen = false) => {
    controller.abort();
    if (!preserveOpen && active === owner) closePicker();
    bindings.delete(button);
  };
  bindings.set(button, {owner, dispose});
  if (preserve) positionPicker();
  return dispose;
}
