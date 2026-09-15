<?php
// A small, safe formatting language for news posts and release notes: everything is escaped first,
// then a few Markdown-style marks become HTML. No raw HTML ever passes through.
//
//   ## Heading          **bold**   *italic*   `code`
//   - list item          [link text](https://example.com)   bare https://links
//   blank line = new paragraph

declare(strict_types=1);

/** Format `$text` as HTML: headings and list items are recognised on any line. */
function markup(string $text): string
{
    $lines = explode("\n", str_replace(["\r\n", "\r"], "\n", trim($text)));
    $html = [];
    $paragraph = [];
    $list = [];
    $flush = function () use (&$html, &$paragraph, &$list) {
        if ($paragraph) {
            $html[] = '<p>' . implode("<br>\n", array_map('markup_inline', $paragraph)) . '</p>';
            $paragraph = [];
        }
        if ($list) {
            $html[] = '<ul class="post-list">' . implode('', array_map(function ($item) {
                return '<li>' . markup_inline($item) . '</li>';
            }, $list)) . '</ul>';
            $list = [];
        }
    };
    foreach ($lines as $line) {
        if (trim($line) === '') {
            $flush();
        } elseif (preg_match('/^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$/', $line, $m)) {
            $flush();
            $html[] = '<h3 class="post-h">' . markup_inline($m[1]) . '</h3>';
        } elseif (preg_match('/^\s*[-*+]\s+(.*)$/', $line, $m)) {
            if ($paragraph) {
                $flush();
            }
            $list[] = $m[1];
        } else {
            if ($list) {
                $flush();
            }
            $paragraph[] = $line;
        }
    }
    $flush();
    return implode("\n", $html);
}

/** Inline marks within one line. */
function markup_inline(string $line): string
{
    $out = '';
    // Split on code spans first, so nothing inside them is formatted.
    $parts = preg_split('/(`[^`]+`)/', $line, -1, PREG_SPLIT_DELIM_CAPTURE);
    foreach ($parts as $part) {
        if (strlen($part) > 2 && $part[0] === '`' && substr($part, -1) === '`') {
            $out .= '<code>' . h(substr($part, 1, -1)) . '</code>';
            continue;
        }
        $out .= markup_links(h($part));
    }
    return $out;
}

/** Links, then bold and italic, on already-escaped text. */
function markup_links(string $escaped): string
{
    $links = [];
    // [text](url): only http(s) and site-relative links.
    $escaped = preg_replace_callback('/\[([^\]]+)\]\(((?:https?:\/\/|\/)[^\s)]+)\)/', function ($m) use (&$links) {
        $links[] = '<a href="' . $m[2] . '" rel="noopener">' . $m[1] . '</a>';
        return "\x01" . (count($links) - 1) . "\x02";
    }, $escaped);
    // Bare https:// links.
    $escaped = preg_replace_callback('/\bhttps?:\/\/[^\s<]+[^\s<.,;:!?)]/', function ($m) use (&$links) {
        $links[] = '<a href="' . $m[0] . '" rel="noopener">' . $m[0] . '</a>';
        return "\x01" . (count($links) - 1) . "\x02";
    }, $escaped);
    $escaped = preg_replace('/\*\*(.+?)\*\*/', '<b>$1</b>', $escaped);
    $escaped = preg_replace('/(?<![*\w])\*(?!\s)(.+?)(?<!\s)\*(?![*\w])/', '<i>$1</i>', $escaped);
    return preg_replace_callback("/\x01(\d+)\x02/", function ($m) use ($links) {
        return $links[(int) $m[1]];
    }, $escaped);
}

/** Plain text for an excerpt: marks removed, cut near `$length` characters at a word. */
function markup_excerpt(string $text, int $length = 220): string
{
    $plain = preg_replace(['/`([^`]+)`/', '/\[([^\]]+)\]\([^)]+\)/', '/[*#]+/', '/^\s*[-*]\s+/m', '/\s+/'], ['$1', '$1', '', '', ' '], $text);
    $plain = trim($plain);
    if (mb_strlen($plain) <= $length) {
        return $plain;
    }
    $cut = mb_substr($plain, 0, $length);
    $space = mb_strrpos($cut, ' ');
    return rtrim(mb_substr($cut, 0, $space !== false && $space > $length * 0.6 ? $space : $length), " ,.;:") . '…';
}
