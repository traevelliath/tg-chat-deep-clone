create table "chat_pair" 
(
    "src_id" integer not null, --references "chat"."id"
    "dst_id" integer not null, --references "chat"."id"

    constraint "chat_pair_pkey" primary key ("src_id", "dst_id")
);

create table "chat"
(
    "id"               integer,
    "channel"          text not null,
    "discussion_group" text     null,

    constraint "chat_pkey" primary key ("id")
);

create table "post"
(
    "id"            integer,
    "chat_id"       integer not null, -- references "chat"."id"
    "discussion_id" integer     null,
    "forwarded"     integer not null default 0,

    constraint "post_pkey" primary key ("id")
);

create table "discussion"
(
    "id"          integer primary key autoincrement,
    "chat_id"     integer not null, -- references "chat"."id"
    "post_id"     integer not null,
    "msg_id"      integer not null,
    "offset_id"   integer not null default 1,
    "offset_date" integer not null default 0,
    "add_offset"  integer not null default -40,
    "limit"       integer not null default 40,
    "max_id"      integer not null default 0,
    "min_id"      integer not null default 0,
    "hash"        integer not null default 0
);

create unique index "discussion_msg_chat_idx" on "discussion" ("msg_id", "chat_id");

create table "message"
(
    "id"         integer,
    "discussion" integer not null, -- references "discussion"."id"
    "forwarded"  integer not null default 0,

    constraint "message_pkey" primary key ("id", "discussion")
);


