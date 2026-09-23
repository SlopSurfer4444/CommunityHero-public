import test from 'node:test';
import assert from 'node:assert/strict';
import {channelBadge} from './social-icons.js';

const platforms = [
  ['VK', 'vk', 'VK'],
  ['youtube', 'youtube', 'YouTube'],
  ['Instagram', 'instagram', 'Instagram'],
  ['Tik Tok', 'tiktok', 'TikTok'],
  ['telegram', 'telegram', 'Telegram'],
];

test('renders every supported platform as an accessible inline SVG', () => {
  for (const [input, slug, label] of platforms) {
    const badge = channelBadge(input);
    assert.match(badge, new RegExp(`class="channel social-icon social-icon--${slug}"`));
    assert.match(badge, new RegExp(`role="img" aria-label="${label}"`));
    assert.match(badge, /<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">/);
    assert.doesNotMatch(badge, /<img|src=|href=/);
  }
});

test('normalizes common English and Russian aliases', () => {
  assert.match(channelBadge('  Vkontakte '), /social-icon--vk/);
  assert.match(channelBadge('ВК'), /social-icon--vk/);
  assert.match(channelBadge('YOU_TUBE'), /social-icon--youtube/);
  assert.match(channelBadge('тик-ток'), /social-icon--tiktok/);
  assert.match(channelBadge('Телега'), /social-icon--telegram/);
});

test('uses a neutral mark and safely labels an unknown channel', () => {
  const badge = channelBadge('Forum <beta> & "friends"');
  assert.match(badge, /social-icon--other/);
  assert.match(badge, /aria-label="Forum &lt;beta&gt; &amp; &quot;friends&quot;"/);
  assert.doesNotMatch(badge, /Forum <beta>/);
});

test('gives an empty channel a useful accessible name', () => {
  assert.match(channelBadge(), /aria-label="Канал"/);
});
