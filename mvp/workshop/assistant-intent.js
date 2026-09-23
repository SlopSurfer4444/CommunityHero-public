// Only a direct top-level operator request can reset a discussion. Never inspect
// quoted comments/materials, and do not infer a reset from merely mentioning one.
export function startsFreshDiscussion(text) {
  if(typeof text!=='string')return false;
  const normalized=text.trim().toLocaleLowerCase('ru').replaceAll('ё','е');
  if(/^["'«>`]/.test(normalized)||/^(?:не|никогда|объясни|что|как|почему)\s/.test(normalized))return false;
  const prefix='(?:(?:бро|брат|пожалуйста|давай|давайте)[, !]*\\s*)*';
  const command='(?:(?:начни|начнем|начнемте|создай|открой|начать)\\s+(?:новый\\s+чат|новое\\s+обсуждение|новую\\s+тему)|(?:начни|начнем|давай)\\s+с\\s+(?:нуля|чистого\\s+листа)|новая\\s+тема|новое\\s+обсуждение)';
  return new RegExp('^'+prefix+command+'(?=$|[\\s.!,:;—-])','u').test(normalized);
}
