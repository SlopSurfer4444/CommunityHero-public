import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantText} from '../workshop/chat-text.js';
test('assistant emphasis retains text and line breaks',()=>{
  assert.equal(assistantText('**Что вижу:**\n• Один\n\nТекст'),'<'+'strong>Что вижу:</strong>\n• Один\n\nТекст');
  assert.equal(assistantText('2 * 3; **не закрыто'),'2 * 3; **не закрыто');
});
test('markup from assistant text cannot create elements or executable links',()=>{
  assert.equal(assistantText('**<img src=x onerror="alert(1)">**'),'<strong>&lt;img src=x onerror=&quot;alert(1)&quot;&gt;</strong>');
  assert.equal(assistantText('<script>x</script> & [x](javascript:x)'), '&lt;script&gt;x&lt;/script&gt; &amp; [x](javascript:x)');
});
