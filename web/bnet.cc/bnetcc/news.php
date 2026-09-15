<?php
// News posts: kept in news.json in the data folder, newest first, pinned posts on top.
//
// A post: id, slug, title, body (markup.php format), author, created, updated, pinned, draft.

declare(strict_types=1);

require_once __DIR__ . '/bootstrap.php';
require_once __DIR__ . '/markup.php';

/** Every post, drafts included, pinned first and then newest first. */
function news_all(): array
{
    $posts = site_read_json(site_data_path('news.json'), []);
    if (!is_array($posts)) {
        return [];
    }
    usort($posts, function ($a, $b) {
        return [(int) !empty($b['pinned']), (int) $b['created']] <=> [(int) !empty($a['pinned']), (int) $a['created']];
    });
    return $posts;
}

/** Published posts only. */
function news_published(): array
{
    return array_values(array_filter(news_all(), function ($p) {
        return empty($p['draft']);
    }));
}

/** A post by id or slug. */
function news_find(string $key): ?array
{
    foreach (news_all() as $post) {
        if ($post['id'] === $key || $post['slug'] === $key) {
            return $post;
        }
    }
    return null;
}

/** A URL-safe slug for a title, unique among `$posts` (other than `$except`). */
function news_slug(string $title, array $posts, string $except = ''): string
{
    $base = strtolower(trim(preg_replace('/[^A-Za-z0-9]+/', '-', $title), '-'));
    if ($base === '') {
        $base = 'post';
    }
    $base = substr($base, 0, 60);
    $slug = $base;
    $n = 2;
    $taken = [];
    foreach ($posts as $p) {
        if ($p['id'] !== $except) {
            $taken[$p['slug']] = true;
        }
    }
    while (isset($taken[$slug])) {
        $slug = $base . '-' . $n++;
    }
    return $slug;
}

/** Create or update a post; the saved post. `$id` empty creates one. */
function news_save(string $id, string $title, string $body, bool $pinned, bool $draft, string $author): array
{
    return site_locked('news', function () use ($id, $title, $body, $pinned, $draft, $author) {
        $posts = site_read_json(site_data_path('news.json'), []);
        if (!is_array($posts)) {
            $posts = [];
        }
        $now = time();
        foreach ($posts as $i => $post) {
            if ($post['id'] === $id) {
                $post['title'] = $title;
                $post['body'] = $body;
                // A published post keeps its address, so links to it keep working.
                if (!empty($post['draft'])) {
                    $post['slug'] = news_slug($title, $posts, $id);
                }
                $post['pinned'] = $pinned;
                $post['draft'] = $draft;
                $post['updated'] = $now;
                // A draft published for the first time takes the time it went out.
                if (!empty($posts[$i]['draft']) && !$draft) {
                    $post['created'] = $now;
                }
                $posts[$i] = $post;
                if (!site_write_json(site_data_path('news.json'), $posts)) {
                    throw new RuntimeException('cannot save the post');
                }
                return $post;
            }
        }
        $post = [
            'id' => bin2hex(random_bytes(8)),
            'slug' => news_slug($title, $posts),
            'title' => $title,
            'body' => $body,
            'author' => $author,
            'created' => $now,
            'updated' => $now,
            'pinned' => $pinned,
            'draft' => $draft,
        ];
        $posts[] = $post;
        if (!site_write_json(site_data_path('news.json'), $posts)) {
            throw new RuntimeException('cannot save the post');
        }
        return $post;
    });
}

/** Delete a post; whether it was there. */
function news_delete(string $id): bool
{
    return site_locked('news', function () use ($id) {
        $posts = site_read_json(site_data_path('news.json'), []);
        $kept = array_values(array_filter(is_array($posts) ? $posts : [], function ($p) use ($id) {
            return $p['id'] !== $id;
        }));
        if (count($kept) === count($posts)) {
            return false;
        }
        if (!site_write_json(site_data_path('news.json'), $kept)) {
            throw new RuntimeException('cannot delete the post');
        }
        return true;
    });
}
