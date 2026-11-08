// All provider-specific DOM access is contained in this adapter.
(config, operation, text) => {
    const visible = el => !!el && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
    const all = selector => Array.from(document.querySelectorAll(selector));
    const inAiMode = new URL(location.href).searchParams.get('udm') === '50' || !!document.querySelector('[data-subtree="aimc"]');
    const input = inAiMode ? all(config.input_selector).filter(visible).filter(el => !el.disabled).at(-1) : null;
    const body = document.body?.innerText || '';
    let error = '';
    if (location.pathname.startsWith('/sorry')) error = 'Google запросил CAPTCHA. Пройдите проверку вручную в Chrome.';
    if (!input && /AI Mode (?:isn.t|is not) available|AI Mode is not supported|Режим ИИ недоступен/i.test(body)) error = 'Google AI Mode недоступен для этого региона или аккаунта.';
    if (all('[role="alert"]').filter(visible).some(el=>/too many requests|quota exceeded|rate limit|лимит.*(?:исчерпан|достигнут)|слишком много запросов/i.test(el.innerText || ''))) error = 'RATE_LIMITED: Google reported a quota/rate limit';
    if (location.hostname === 'consent.google.com') error = 'В Chrome открыт экран согласия Google. Завершите его вручную.';
    if (operation === 'fill') {
        if (!input) throw new Error('Поле AI Mode не найдено');
        input.focus();
        if (input instanceof HTMLTextAreaElement || input instanceof HTMLInputElement) {
            input.select();
        } else {
            const range = document.createRange();
            range.selectNodeContents(input);
            const selection = getSelection();
            selection.removeAllRanges(); selection.addRange(range);
        }
        return true;
    }
    if (operation === 'verify') {
        if (!input) throw new Error('Поле AI Mode исчезло');
        if ((input.value ?? input.textContent).trim() !== text.trim()) throw new Error('Страница не приняла текст сообщения');
        return true;
    }
    if (operation === 'submit') {
        const send = all('button, [role="button"]').find(el => visible(el) && !el.disabled && el.getAttribute('aria-disabled') !== 'true'
            && /^(?:send|submit|отправить)(?:\s+(?:message|prompt|query|сообщение|запрос))?$/i.test((el.getAttribute('aria-label') || el.getAttribute('title') || '').trim()));
        if (send) { send.click(); return true; }
        return false;
    }
    if (operation === 'diagnostics') {
        return {inAiMode, udm:new URL(location.href).searchParams.get('udm'), ready:!!input, draftLength:input?.value?.length || 0, url: location.origin + location.pathname,
            sendControls: all('button, [role="button"]').filter(el=>visible(el) && /^(send|submit|отправить|поиск|search|stop|остановить)/i.test(el.getAttribute('aria-label') || '')).map(el=>({label:el.getAttribute('aria-label'), type:el.getAttribute('type'), disabled:el.disabled})),
            contentTags: all('pre, code, [role="heading"]').map(el=>({tag:el.tagName,class:el.className,role:el.getAttribute('role'),parentClass:el.parentElement?.className})),
            inputs: all('textarea, [contenteditable="true"]').map(el => ({tag:el.tagName, name:el.getAttribute('name'), role:el.getAttribute('role'), visible:visible(el)})),
            responseCandidates: all(config.response_selector).map(el => ({tag:el.tagName, class:el.className, visible:visible(el), complete:el.getAttribute('data-complete')})), error};
    }
    const candidates = all(config.response_selector).filter(visible);
    // Google streams paragraphs/code as separate .n6owBd blocks. Group them by
    // the enclosing turn container; counting paragraphs as replies loses content.
    const blocks = candidates.filter(el => !candidates.some(parent => parent !== el && parent.contains(el)));
    const responses = [];
    for (const block of blocks) {
        let root = block.closest('[data-container-id]');
        if (!root || root.getAttribute('data-container-id') === 'main-col') root = block;
        let group = responses.find(group => group.root === root);
        if (!group) { group = {root, blocks:[]}; responses.push(group); }
        group.blocks.push(block);
    }
    const response = responses.at(-1);
    const escape = value => value.replace(/[\\`*_[\]]/g, '\\$&');
    function markdown(node) {
        if (node.nodeType === Node.TEXT_NODE) return escape(node.textContent || '');
        if (node.nodeType !== Node.ELEMENT_NODE) return '';
        const tag = node.tagName.toLowerCase();
        if (['button','script','style','svg'].includes(tag) || node.hasAttribute('data-ignore-copy') || node.getAttribute('data-xid') === 'Gd7Hsc' || node.getAttribute('role') === 'button' || node.getAttribute('aria-hidden') === 'true') return '';
        const children = () => Array.from(node.childNodes).map(markdown).join('');
        if (tag === 'pre') return '\n```\n' + node.innerText.trim() + '\n```\n\n';
        if (tag === 'code') return '`' + node.textContent + '`';
        if (/^h[1-6]$/.test(tag)) return '\n' + '#'.repeat(Number(tag[1])) + ' ' + children().trim() + '\n\n';
        if (node.getAttribute('role') === 'heading') return '\n' + '#'.repeat(Math.min(6, Math.max(1, Number(node.getAttribute('aria-level')) || 3))) + ' ' + children().trim() + '\n\n';
        if (tag === 'strong' || tag === 'b') return '**' + children() + '**';
        if (tag === 'em' || tag === 'i') return '*' + children() + '*';
        if (tag === 'a') {
            const label = children().trim();
            const url = node.href;
            return label && /^https?:/.test(url) ? '[' + label + '](' + url.replace(/\)/g, '%29') + ')' : label;
        }
        if (tag === 'ul' || tag === 'ol') return '\n' + Array.from(node.children).filter(el => el.tagName === 'LI').map((el,i) => (tag === 'ol' ? `${i+1}. ` : '* ') + markdown(el).trim().replace(/\n/g, '\n    ')).join('\n') + '\n\n';
        if (tag === 'table') {
            const rows = Array.from(node.querySelectorAll('tr')).map(row => Array.from(row.querySelectorAll('th,td')).map(cell => markdown(cell).trim().replace(/\n/g,' ').replace(/\|/g,'\\|')));
            return '\n' + rows.map((row,i) => '| ' + row.join(' | ') + ' |' + (i === 0 ? '\n| ' + row.map(()=>'---').join(' | ') + ' |' : '')).join('\n') + '\n\n';
        }
        if (tag === 'br') return '\n';
        if (['p','div','section','li'].includes(tag)) return children() + '\n\n';
        return children();
    }
    const scope = response?.root;
    const finalMarker = scope?.querySelector('[data-xid="Gd7Hsc"][data-complete="true"]');
    const hasActions = !!scope && Array.from(scope.querySelectorAll(config.complete_selector)).some(visible);
    const nativeBlocks = !!response && (response.blocks.some(block=>block.matches('.n6owBd')) || (!!scope.getAttribute('data-container-id') && scope.getAttribute('data-container-id') !== 'main-col'));
    const complete = !!response && (nativeBlocks ? !!finalMarker || hasActions
        : scope.getAttribute('data-complete') === 'true' || hasActions);
    return { ready: !!input, draft: !!(input?.value ?? input?.textContent ?? '').trim(), text: scope ? markdown(scope).replace(/\n{3,}/g,'\n\n').trim() : '',
        generating: all(config.stop_selector).some(visible),
        response_count: responses.length, busy: all(config.stop_selector).some(visible)
            || (nativeBlocks && !complete),
        complete,
        error, url: location.origin + location.pathname };
}
