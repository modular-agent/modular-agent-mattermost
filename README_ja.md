# Mattermost Agents for Modular Agent

Modular Agent 用の Mattermost 連携エージェント。メッセージ送信、履歴取得、チャンネル一覧、WebSocket によるリアルタイムイベント受信を提供します。

[English](README.md) | 日本語

## 機能一覧

- **Post** — Mattermost チャンネルへのテキスト送信、スレッド返信、画像送信
- **History** — チャンネルのメッセージ履歴を取得
- **Channels** — ボットユーザーが所属するチャンネル一覧を取得
- **Listener** — WebSocket でリアルタイムにメッセージを受信（自動再接続付き）
- **ToMessage** — Mattermost メッセージオブジェクトを LLM 用 Message 形式に変換

## セットアップ

### グローバル設定または環境変数

| グローバル設定 | 環境変数 | 説明 |
| -------------- | -------- | ---- |
| `mattermost_token` | `MATTERMOST_TOKEN` | Bot トークンまたはパーソナルアクセストークン |
| `server_url` | `MATTERMOST_URL` | Mattermost サーバーURL（例: `https://mattermost.example.com`） |

### Bot アカウントの作成

1. **システムコンソール > 統合機能 > Bot アカウント** で Bot アカウントが有効になっていることを確認
2. **統合機能 > Bot アカウント > Bot アカウントを追加** から Bot を作成
3. 生成されたアクセストークンをコピー — これが `mattermost_token` になります

または、**プロフィール > セキュリティ > パーソナルアクセストークン** からパーソナルアクセストークンを作成することもできます。

### ID の確認方法

- **チャンネル ID**: チャンネルを開き、チャンネル名のヘッダーをクリックすると表示されるダイアログから ID をコピーできます。または Channels エージェントを使って全チャンネルの ID を一覧取得できます。
- **チーム ID**: API（トークン付きで `GET /api/v4/teams`）または **システムコンソール > チーム**（管理者のみ）から確認できます。

## Feature Flags

| Feature | デフォルト | 説明 |
| ------- | ---------- | ---- |
| `image` | Yes | Post と Listener エージェントの画像アップロード・ダウンロード対応 |

## Mattermost/Post

Mattermost チャンネルにメッセージを送信します。Mattermost は標準 Markdown をネイティブサポートしているため、Markdown はそのまま渡されます。

### 設定

| 設定 | 型 | デフォルト | 説明 |
| ---- | -- | ---------- | ---- |
| `channel_id` | string | "" | 送信先の Mattermost チャンネル ID |

### ポート

- **入力**: `message` — String、Message、`text`/`root_id` フィールドを持つオブジェクト、配列、または画像（AgentValue::Image）
- **出力**: `result` — 成功時に `ok`、`post_id`、`channel` を含むオブジェクト

### 画像アップロード

画像（AgentValue::Image）または画像が添付された Message を受信すると、PNG ファイルとしてアップロードし投稿に添付します。`image` feature flag（デフォルトで有効）が必要です。

### スレッド返信

- 入力オブジェクトの `root_id` でスレッド返信先の親投稿 ID を指定します
- 出力は `post_id` キーに投稿 ID を格納します

## Mattermost/History

Mattermost チャンネルのメッセージ履歴を新しい順に取得します。

### 設定

| 設定 | 型 | デフォルト | 説明 |
| ---- | -- | ---------- | ---- |
| `channel_id` | string | "" | 履歴を取得する Mattermost チャンネル ID |
| `limit` | integer | 10 | 取得する投稿の最大数 |

### ポート

- **入力**: `trigger` — 任意の値で履歴取得をトリガー
- **出力**: `values` — `text`、`user`、`post_id`、`root_id`、`create_at` フィールドを持つ投稿オブジェクトの配列

> **注意:** 出力ポート名は `values` です。標準メッセージフィールドに加えて Mattermost 固有の `create_at` フィールドを含むためです。

Mattermost の投稿 ID は時系列ソートに使えないため、`create_at`（ミリ秒タイムスタンプ）を出力に含みます。

## Mattermost/Channels

ボットユーザーが所属するチャンネルの一覧を取得します。`team_id` が未設定の場合、ユーザーが所属する最初のチームを自動的に使用します。

### 設定

| 設定 | 型 | デフォルト | 説明 |
| ---- | -- | ---------- | ---- |
| `team_id` | string | "" | Mattermost チーム ID（未設定の場合は自動検出） |
| `limit` | integer | 100 | 返すチャンネルの最大数 |

### ポート

- **入力**: `trigger` — 任意の値でチャンネル一覧取得をトリガー
- **出力**: `channels` — `id`、`name`、`is_private`、`is_archived`、`topic`、`purpose` フィールドを持つチャンネルオブジェクトの配列

## Mattermost/Listener

ソースエージェント（入力なし）。WebSocket 経由で Mattermost のメッセージをリアルタイムに受信します。ワークフロー開始時にリスニングを開始し、メッセージを受信するたびに出力します。切断時は指数バックオフ（1秒〜30秒）で自動再接続します。認証失敗時はリトライを停止します。

### 設定

| 設定 | 型 | デフォルト | 説明 |
| ---- | -- | ---------- | ---- |
| `channel_id` | string | "" | チャンネルフィルター（未設定の場合は全チャンネルを受信） |

### ポート

- **出力**: `value` — `message`（Message）、`user`、`channel`、`post_id`、`root_id` を含むオブジェクト

## Mattermost/ToMessage

Mattermost のメッセージオブジェクトを LLM エージェント向けの AgentValue::Message 形式に変換します。

### ポート

- **入力**: `value` — 単一のメッセージオブジェクトまたはメッセージオブジェクトの配列
- **出力**: `message` — AgentValue::Message または AgentValue::Message の配列

## アーキテクチャ

- REST API v4 は `reqwest` で直接実装（サードパーティの Mattermost SDK は不使用）
- リアルタイムイベントは `tokio-tungstenite` による WebSocket 接続
- `MattermostClient` は `(server_url, token)` ペアごとに `OnceLock<Mutex<BTreeMap>>` でキャッシュ

## 主要な依存クレート

- [reqwest](https://crates.io/crates/reqwest) — REST API v4 用 HTTP クライアント
- [tokio-tungstenite](https://crates.io/crates/tokio-tungstenite) — リアルタイムイベント用 WebSocket クライアント
- [chrono](https://crates.io/crates/chrono) — 画像ファイル名のタイムスタンプ生成

## ライセンス

Apache-2.0 OR MIT
