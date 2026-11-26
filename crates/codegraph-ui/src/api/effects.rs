//! Effects — calls that leave the index and change something outside the
//! process — for the Steps view. Upstream `src/ui-server/api/effects.ts`
//! (`e8ddc4e2`).
//!
//! A curated table, deliberately: "any call into a package" is every `Date`
//! and `Math.max`, and a box for each would bury the ones that matter. Rows
//! match the CALL AS WRITTEN — the whole member chain read from the source
//! (`prisma.article.findFirst`, `res.status(404).json`), because the index
//! keeps only the last segment of a deep chain — and, for a few families, the
//! declared type of the receiver (`OwnerRepository owners` → `owners.save` is
//! the database). The categories are the reader's: a database write, a
//! response, a job on a queue, an email, a charge, a cache read, a token
//! check, a call to another service, a file, a process. Language families
//! gate the rows that mean something else elsewhere; the first match wins.
//!
//! The rows are upstream's patterns, with JavaScript's ASCII `\w`, `\d` and
//! `\b` spelled out. Four of upstream's patterns look around (`effects.ts`
//! 122, 123, 377 and 407); the `regex` crate has no lookaround, so those
//! checks are code: a model receiver that is not a view model, a static ORM
//! call whose receiver is not a built-in, an interface's `I` prefix, and an
//! `Http` prefix before a status name.

use std::sync::OnceLock;

use codegraph_core::types::Language;
use regex::Regex;
use serde::Serialize;

/// What a call does to the world outside the process, in the reader's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectCategory {
    Network,
    Storage,
    Device,
    Telemetry,
    Database,
    Response,
    Queue,
    Email,
    Payments,
    Cache,
    Auth,
    Process,
}

impl EffectCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Storage => "storage",
            Self::Device => "device",
            Self::Telemetry => "telemetry",
            Self::Database => "database",
            Self::Response => "response",
            Self::Queue => "queue",
            Self::Email => "email",
            Self::Payments => "payments",
            Self::Cache => "cache",
            Self::Auth => "auth",
            Self::Process => "process",
        }
    }
}

/// What kind of project the picture is of: an app (screens), an API
/// (endpoints) or a web app (both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    App,
    Api,
    Web,
}

/// The edge a call arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Calls,
    Instantiates,
}

/// Read or write, from a database call's method name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

/// What a call is, when the table names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Effect {
    pub category: EffectCategory,
    /// The table, model or collection, when the call names it: `user`,
    /// `Owner`, `TodoItems`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Only for `database`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
}

impl Effect {
    fn of(category: EffectCategory) -> Self {
        Self {
            category,
            model: None,
            access: None,
        }
    }
}

/// One call to classify (upstream `EffectInput`).
#[derive(Debug, Clone, Copy)]
pub struct EffectInput<'a> {
    /// The call as written (the whole member chain), or the reference name
    /// the index kept.
    pub text: &'a str,
    pub kind: CallKind,
    pub language: Option<Language>,
    pub project: ProjectKind,
    /// The declared type of the receiver, when the graph has it.
    pub receiver_type: Option<&'a str>,
    /// The argument list, abbreviated, when it was read.
    pub args: Option<&'a str>,
    /// The receiver is a type the project declares: a library name the table
    /// keys on is shadowed then; only the database and response rows apply.
    pub project_type: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Js,
    Py,
    Jvm,
    Cs,
    Go,
    C,
    Rb,
    Php,
    Swift,
    Rs,
}

fn family_of(language: Option<Language>) -> Option<Family> {
    Some(match language? {
        Language::JavaScript | Language::TypeScript | Language::Tsx | Language::Jsx => Family::Js,
        Language::Python => Family::Py,
        Language::Java | Language::Kotlin | Language::Scala => Family::Jvm,
        Language::CSharp => Family::Cs,
        Language::Go => Family::Go,
        Language::C | Language::Cpp | Language::ObjC => Family::C,
        Language::Ruby => Family::Rb,
        Language::Php => Family::Php,
        Language::Swift => Family::Swift,
        Language::Rust => Family::Rs,
        _ => return None,
    })
}

/// How a row recognises a call.
enum Test {
    Pattern(&'static str),
    /// Upstream's `…\w*(?<![Vv]iew)(?:Model|Entity|…)\.…` (`effects.ts:122`):
    /// a model's receiver, unless it is a view model.
    ModelNotViewModel,
    /// Upstream's `^(?!BUILTIN\b)[A-Z]\w*\.ORM_OP$` (`effects.ts:123`): a
    /// static ORM call, unless the receiver is a built-in.
    OrmStaticNotBuiltin,
}

struct EffectRule {
    category: EffectCategory,
    test: Test,
    /// Families the row applies to; empty = every language.
    only: &'static [Family],
    /// Only for `new X(…)` / a constructor call.
    instantiates: bool,
}

/// Upstream `EFFECT_RULES` (`effects.ts:80-223`), first match wins.
const EFFECT_RULES: &[EffectRule] = &[
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:res|response|reply|rep|ctx|c|context)(?:\.(?:status|sendStatus|code|type|header|headers|set|append|cookie|clearCookie|vary|location|links|format))*\.(?:status|json|jsonp|send|sendStatus|end|redirect|render|sendFile|download|attachment|write|writeHead|code|type|header|set|cookie|body|text|html|notFound|stream|file|view|throw|assert)$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:NextResponse|Response)\.(?:json|redirect|rewrite|next|error)$|^(?:createError|createHttpError|httpErrors\.[0-9A-Za-z_]+|Boom\.[0-9A-Za-z_]+|boom\.[0-9A-Za-z_]+|HttpError|HTTPError)$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(r#"^(?:NextResponse|Response)$"#),
        only: &[Family::Js],
        instantiates: true,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:HTTPException|JSONResponse|Response|RedirectResponse|StreamingResponse|FileResponse|HTMLResponse|PlainTextResponse|ORJSONResponse|jsonify|abort|make_response|redirect|render_template|send_file|send_from_directory|JsonResponse|HttpResponse|HttpResponseRedirect|HttpResponsePermanentRedirect|HttpResponseNotFound|HttpResponseBadRequest|HttpResponseForbidden|HttpResponseNotAllowed|HttpResponseServerError|Http404|render|get_object_or_404|get_list_or_404|NotFound|PermissionDenied|ValidationError|AuthenticationFailed|NotAuthenticated|ParseError|MethodNotAllowed|Throttled|APIException|status\.HTTP_[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^ResponseEntity(?:\.[0-9A-Za-z_]+)*$|^ResponseStatusException$|^(?:ServerResponse|Mono\.just\(ResponseEntity)"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(r#"^(?:ModelAndView|RedirectView)$"#),
        only: &[Family::Jvm],
        instantiates: true,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:Ok|NotFound|BadRequest|Created|CreatedAtAction|CreatedAtRoute|NoContent|Unauthorized|Forbid|Accepted|AcceptedAtAction|Problem|StatusCode|Conflict|UnprocessableEntity|Redirect|RedirectPermanent|RedirectToAction|RedirectToPage|RedirectToRoute|LocalRedirect|View|PartialView|Json|File|PhysicalFile|Content|Challenge|SignIn|SignOut|ValidationProblem|Page)$|^(?:Results|TypedResults)\.[0-9A-Za-z_]+$|^(?:Response|HttpContext\.Response)\.(?:WriteAsync|WriteAsJsonAsync|Redirect|StatusCode)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:c|ctx|g)\.(?:JSON|IndentedJSON|PureJSON|AsciiJSON|SecureJSON|JSONP|XML|YAML|TOML|ProtoBuf|String|HTML|Data|DataFromReader|File|FileAttachment|Redirect|Render|Status|AbortWithStatus|AbortWithStatusJSON|AbortWithError|Abort|Error|SendString|SendStatus|Send|Blob|Stream|NoContent|Attachment|Format)$|^http\.(?:Error|Redirect|NotFound|ServeFile|ServeContent)$|^(?:w|rw|res|writer)\.(?:WriteHeader|Write|Header)$|^(?:render|json|xml)\.[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:Abort|Response|HTTPStatus\.[0-9A-Za-z_]+|req\.redirect|request\.redirect)$"#,
        ),
        only: &[Family::Swift],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:render|redirect_to|redirect_back|head|respond_to|respond_with|send_data|send_file|render_to_string)$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:response|abort|abort_if|abort_unless|redirect|view|back|json|to_route)(?:->[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Response,
        test: Test::Pattern(
            r#"^(?:HttpResponse|Json|StatusCode|Redirect|NamedFile|HttpResponseBuilder)(?:::[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Rs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:self\.)?(?:[0-9A-Za-z_]*[kK]eychain|Keychain[0-9A-Za-z_]*)(?:\.[0-9A-Za-z_]+)+$"#,
        ),
        only: &[Family::Swift],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:prisma|db|database|orm|em|entityManager|dataSource|queryRunner|knex|kysely|sequelize|mongoose|drizzle|sql|pool|pg|pgClient|conn|connection|repo|repository|collection|trx|tx|typeorm|dbClient|mongo|mongoClient)\.(?:[0-9A-Za-z_]+\.)?\$?[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:this\.)?_?[0-9A-Za-z_]*(?:Repository|Repo|Dao|DAO|Mapper|EntityManager|DataSource|Knex|Prisma|Kysely|Drizzle|Sequelize|DbContext|DbSet)\.(?:[0-9A-Za-z_]+\.)?[0-9A-Za-z_]+$"#,
        ),
        only: &[
            Family::Js,
            Family::Jvm,
            Family::Cs,
            Family::Go,
            Family::Rs,
            Family::Swift,
            Family::Rb,
        ],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::ModelNotViewModel,
        only: &[
            Family::Js,
            Family::Jvm,
            Family::Cs,
            Family::Go,
            Family::Rs,
            Family::Swift,
            Family::Rb,
        ],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::OrmStaticNotBuiltin,
        only: &[Family::Js, Family::Rb, Family::Swift, Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:self\.)?(?:[0-9A-Za-z_]*_)?(?:session|db|database|engine|cursor|conn|connection|Session)\.(?:session\.)?(?:query|add|add_all|commit|execute|executemany|exec|delete|refresh|flush|rollback|get|merge|scalars?|scalar_one[0-9A-Za-z_]*|one|one_or_none|first|all|begin|close|expunge|bulk_[0-9A-Za-z_]+|select|insert|update|fetchone|fetchall|fetchmany|create_all|drop_all|run_sync)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^[A-Z][0-9A-Za-z_]*\.(?:objects|query|_default_manager)(?:\.[0-9A-Za-z_]+)*$|^[0-9A-Za-z_]+\.objects\.[0-9A-Za-z_]+$|^(?:select|insert|update|delete|text|func\.[0-9A-Za-z_]+|bulk_create|bulk_update|get_object_or_404)$|^[0-9A-Za-z_]+\.(?:save|delete|refresh_from_db|get_or_create|update_or_create|filter|exclude|annotate|aggregate|values|values_list|select_related|prefetch_related|bulk_create|bulk_update|create|update|count|exists)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[rR]epository|[0-9A-Za-z_]*[rR]epo|[0-9A-Za-z_]*Dao|[0-9A-Za-z_]*DAO|[0-9A-Za-z_]*Mapper|jdbcTemplate|namedParameterJdbcTemplate|jdbc|entityManager|em|session|sessionFactory|mongoTemplate|mongoOperations|r2dbcEntityTemplate|databaseClient|criteriaBuilder|query|typedQuery|nativeQuery|jpaRepository|crudRepository|dsl|dslContext|create|template)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:this\.)?_?(?:[0-9A-Za-z_]*[rR]epository|[0-9A-Za-z_]*[rR]epo|[0-9A-Za-z_]*Dao|[0-9A-Za-z_]*[cC]ontext|dbContext|db|_db|_context|connection|_connection|conn|_conn|collection|_collection|session|_session|unitOfWork|_unitOfWork|uow|_uow|dbSet|_dbSet)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"\.(?:SaveChanges|SaveChangesAsync|ToListAsync|ToArrayAsync|FirstOrDefaultAsync|SingleOrDefaultAsync|FirstAsync|SingleAsync|AnyAsync|CountAsync|ExecuteUpdateAsync|ExecuteDeleteAsync|ExecuteSqlRawAsync|ExecuteSqlAsync|FromSqlRaw|FromSql|AddAsync|AddRangeAsync|FindAsync|QueryAsync|QueryFirstOrDefaultAsync|QuerySingleAsync|ExecuteAsync|ExecuteScalarAsync|InsertOneAsync|InsertManyAsync|ReplaceOneAsync|UpdateOneAsync|DeleteOneAsync|DeleteManyAsync)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:db|DB|tx|conn|pool|dbConn|client|repo|store|coll|collection|session|s\.db|r\.db|h\.db|s\.DB|h\.DB|a\.db|app\.db|q|queries|s\.queries|gorm|sqlx|dbx)\.(?:[0-9A-Za-z_]+\.)*(?:Query[0-9A-Za-z_]*|Exec[0-9A-Za-z_]*|Prepare[0-9A-Za-z_]*|Begin[0-9A-Za-z_]*|Create|First|Find|Save|Delete|Where|Updates?|Model|Table|Raw|Scan|Get|Select|NamedExec|InsertOne|InsertMany|FindOne|UpdateOne|UpdateMany|DeleteOne|DeleteMany|ReplaceOne|CountDocuments|Count|Take|Last|Preload|Joins|Order|Limit|Offset|Transaction|AutoMigrate|Migrate|Insert|Update|Upsert|Aggregate|Distinct|Pluck|Rows|Row|Set|Get[0-9A-Za-z_]+|List[0-9A-Za-z_]+|Create[0-9A-Za-z_]+|Update[0-9A-Za-z_]+|Delete[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^[A-Z][0-9A-Za-z_]*\.(?:query|find|create|all|first|last|save|delete|update)$|^[0-9A-Za-z_]+\.(?:save|create|delete|update|query)$|^(?:req|request)\.db\.[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Swift],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:@?[0-9A-Za-z_]+)\.(?:save|save!|update|update!|update_attributes|destroy|destroy!|reload|touch|increment!|decrement!)$|^ActiveRecord::Base\.[0-9A-Za-z_]+$|^[0-9A-Za-z_]+\.(?:where|find|find_by|find_each|first|last|all|create|create!|pluck|count|exists\?|order|includes|joins|delete_all|update_all|insert_all|upsert_all|find_or_create_by)$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^[A-Z][0-9A-Za-z_]*::(?:find|findOrFail|findMany|create|firstOrCreate|updateOrCreate|where|whereIn|all|first|firstOrFail|query|insert|update|destroy|truncate|count|with|select|orderBy|paginate)$|^DB::[0-9A-Za-z_]+$|^\$[0-9A-Za-z_]+->(?:save|delete|update|create|fill|refresh|forceDelete|restore|increment|decrement|touch)$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:sqlx|diesel|sea_orm|mongodb)(?:::[0-9A-Za-z_]+)*$|^[0-9A-Za-z_]+(?:::[0-9A-Za-z_]+)*::(?:find|find_by_id|insert|update|delete|save|find_many|find_one|filter)$|^(?:conn|pool|tx|db|client)\.(?:execute|query[0-9A-Za-z_]*|prepare|begin|commit|rollback|fetch[0-9A-Za-z_]*)$"#,
        ),
        only: &[Family::Rs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Database,
        test: Test::Pattern(
            r#"^(?:sqlite3_[0-9A-Za-z_]+|mysql_[0-9A-Za-z_]+|PQ[0-9A-Za-z_]+|SQLExec[0-9A-Za-z_]*|SQLPrepare|SQLFetch[0-9A-Za-z_]*|redis[0-9A-Za-z_]*Command|mongoc_[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::C],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[qQ]ueue[0-9A-Za-z_]*)\.(?:add|addBulk|process|createJob|send|sendMessage|publish|push|enqueue)$|^(?:this\.)?(?:sqs|sqsClient|sns|snsClient|pubsub|topic|producer|kafka|kafkaProducer|rabbit|channel|amqp|nats|nc|eventBridge|bus|messageBus|eventBus|agenda|boss|pgBoss|inngest|trigger|client\.queue)\.(?:[0-9A-Za-z_]+\.)*(?:send|sendMessage|sendMessageBatch|publish|publishMessage|produce|emit|add|schedule|now|enqueue|sendToQueue|put|dispatch|trigger|createJob|createSchedule|invoke|batch)$|^(?:agenda|boss|pgBoss|inngest)\.[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:SendMessageCommand|PublishCommand|PutEventsCommand|SendMessageBatchCommand|InvokeCommand)$"#,
        ),
        only: &[Family::Js],
        instantiates: true,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^[0-9A-Za-z_]+\.(?:delay|apply_async|send_task|si|s|enqueue|enqueue_call|enqueue_in|enqueue_at|send_message|send_message_batch|basic_publish|publish|produce|put_events|invoke)$|^(?:celery|app|current_app)\.send_task$|^(?:sqs|sns|producer|channel|queue|q|redis_queue|dramatiq|huey)\.[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[tT]emplate|[0-9A-Za-z_]*[pP]ublisher|[0-9A-Za-z_]*[pP]roducer|[0-9A-Za-z_]*[qQ]ueue[0-9A-Za-z_]*|sqsClient|snsClient|amazonSQS|amazonSNS|eventBus|messageBus|bus|channel|rabbitTemplate|kafkaTemplate|jmsTemplate|streamBridge|eventPublisher|applicationEventPublisher)\.(?:send[0-9A-Za-z_]*|convertAndSend|publish[0-9A-Za-z_]*|publishEvent|produce|sendMessage|put[0-9A-Za-z_]*|emit|dispatch|enqueue)$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:this\.)?_?(?:bus|publishEndpoint|sendEndpoint|producer|queue[0-9A-Za-z_]*|channel|messageSession|serviceBus|serviceBusSender|topicClient|queueClient|eventGrid|BackgroundJob|RecurringJob|BackgroundJobClient|jobClient|_jobs|jobs)\.(?:[0-9A-Za-z_]+\.)*(?:Publish[0-9A-Za-z_]*|Send[0-9A-Za-z_]*|Produce[0-9A-Za-z_]*|Enqueue[0-9A-Za-z_]*|Schedule[0-9A-Za-z_]*|AddOrUpdate|BasicPublish|SendMessageAsync|SendMessagesAsync|Create[0-9A-Za-z_]*Message|Dispatch[0-9A-Za-z_]*|Trigger[0-9A-Za-z_]*)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:[0-9A-Za-z_]+\.)*(?:Publish|PublishMsg|PublishAsync|SendMessage|SendMessageWithContext|Produce|ProduceSync|Enqueue|EnqueueContext|PutEvents|WriteMessages|Emit|Dispatch|Schedule|SendMsg)$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^[0-9A-Za-z_]+\.(?:perform_later|perform_async|perform_in|perform_at|deliver_later|set|publish|enqueue)$|^Sidekiq::Client\.[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:dispatch|dispatch_now|dispatch_sync|Queue::[0-9A-Za-z_]+|Bus::[0-9A-Za-z_]+|Event::dispatch|event|broadcast)$|^[0-9A-Za-z_]+::dispatch(?:Sync|Now|AfterResponse)?$|^\$[0-9A-Za-z_]+->dispatch$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[mM]ail[0-9A-Za-z_]*|[0-9A-Za-z_]*[tT]ransporter|sgMail|sendgrid|resend|mailgun|postmark|ses|sesClient|sesv2|smtp|nodemailer|courier|brevo|sendinblue|mailjet|mandrill|loops|plunk)\.(?:[0-9A-Za-z_]+\.)*(?:send[0-9A-Za-z_]*|sendMail|sendEmail|sendTemplate|create|deliver|emails\.send|transactional[0-9A-Za-z_]*)$|^(?:SendEmailCommand|SendTemplatedEmailCommand|SendRawEmailCommand)$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^(?:send_mail|send_mass_mail|mail_admins|mail_managers|EmailMessage|EmailMultiAlternatives|mail\.send|mail\.send_message|smtplib\.SMTP|smtplib\.SMTP_SSL|ses\.send_email|ses\.send_raw_email|sg\.send|sendgrid\.[0-9A-Za-z_]+|resend\.Emails\.send|postmark\.[0-9A-Za-z_]+)$|^[0-9A-Za-z_]+\.send_(?:email|mail|message)$|^(?:smtp|server|mailer|email_client)\.(?:sendmail|send_message|send|login|starttls)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[mM]ailSender|[0-9A-Za-z_]*[mM]ailer|mailService|emailService|sesClient|amazonSimpleEmailService|transport|Transport)\.(?:send[0-9A-Za-z_]*|deliver)$|^Transport\.send$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^(?:this\.)?_?(?:emailSender|emailService|mailService|mailer|smtpClient|sendGridClient|sesClient|fluentEmail|email)\.(?:Send[0-9A-Za-z_]*)$|^Email\.(?:From|Send[0-9A-Za-z_]*)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^(?:smtp\.SendMail|mail\.Send[0-9A-Za-z_]*|[0-9A-Za-z_]+\.SendEmail[0-9A-Za-z_]*|[0-9A-Za-z_]+\.SendMail|ses\.SendEmail|sg\.Send|mg\.Send)$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^[0-9A-Za-z_]+Mailer\.[0-9A-Za-z_]+$|^[0-9A-Za-z_]+\.(?:deliver_now|deliver_later|deliver)$|^Mail\.deliver$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Email,
        test: Test::Pattern(
            r#"^Mail::(?:to|send|raw|queue|bcc|cc)$|^Notification::send$|^\$[0-9A-Za-z_]+->notify$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Payments,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:stripe|Stripe|braintree|paypal|PayPal|square|razorpay|paddle|Paddle|adyen|mollie|chargebee|recurly|lemonSqueezy|lemonsqueezy|checkout|gateway|paymentGateway|paymentsClient|paymentService|_paymentService|stripeClient|stripeService|_stripe)(?-u:\b)"#,
        ),
        only: &[],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Cache,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:redis|redisClient|ioredis|cache|cacheManager|cacheService|memcached|memcache|kv|KV|upstash|_cache|_distributedCache|_memoryCache|distributedCache|memoryCache|redisTemplate|stringRedisTemplate|jedis|lettuce|redisson|rdb|rc|cacheClient|caches|Cache|env\.[0-9A-Za-z_]*KV[0-9A-Za-z_]*)\.(?:[0-9A-Za-z_]+\.)*(?:get[0-9A-Za-z_]*|set[0-9A-Za-z_]*|del[0-9A-Za-z_]*|delete[0-9A-Za-z_]*|remove[0-9A-Za-z_]*|incr[0-9A-Za-z_]*|decr[0-9A-Za-z_]*|expire[0-9A-Za-z_]*|exists|has|hget[0-9A-Za-z_]*|hset[0-9A-Za-z_]*|hdel|hgetall|lpush|rpush|lpop|rpop|sadd|srem|smembers|zadd|zrange[0-9A-Za-z_]*|zrem|wrap|mget|mset|setex|setnx|ttl|keys|flush[0-9A-Za-z_]*|reset|clear|store|put[0-9A-Za-z_]*|evict|invalidate[0-9A-Za-z_]*|GetOrCreate[0-9A-Za-z_]*|GetString[0-9A-Za-z_]*|SetString[0-9A-Za-z_]*|GetOrSet[0-9A-Za-z_]*|Refresh[0-9A-Za-z_]*|Get|Set|Del|Delete|Exists|Expire|Incr|Decr|HGet[0-9A-Za-z_]*|HSet[0-9A-Za-z_]*|Publish|Subscribe|TryGetValue|CreateEntry|opsForValue|opsForHash|opsForList|opsForSet|Fetch|fetch|read|write|delete_pattern|delete_many|get_many|set_many|touch|add|remember|rememberForever|forget|pull|increment|decrement|forever|tags|memoize)$"#,
        ),
        only: &[],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:jwt|jsonwebtoken|jose|SignJWT|jwtVerify|bcrypt|bcryptjs|argon2|scrypt|passport|jwtService|_jwtService|tokenService|authService\.sign[0-9A-Za-z_]*|auth\.api|auth\.[0-9A-Za-z_]+|betterAuth|clerk|clerkClient|supabase\.auth|firebase\.auth|admin\.auth|getAuth|getServerSession|getSession|auth|lucia|nextauth|NextAuth|verifyIdToken|verifyToken|signToken|createSession|invalidateSession|oauth2|OAuth2Client|okta|auth0|cognito|CognitoIdentityServiceProvider|Keychain|crypto\.timingSafeEqual|crypto\.pbkdf2[0-9A-Za-z_]*|crypto\.scrypt[0-9A-Za-z_]*|crypto\.createHmac)(?-u:\b)(?:\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+)?$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:jwt|pyjwt|jose|bcrypt|argon2|passlib|pwd_context|password_hasher|hashers|check_password|make_password|authenticate|login|logout|login_required|get_user|verify_password|get_password_hash|create_access_token|create_refresh_token|decode_token|OAuth2PasswordBearer|HTTPBearer|HTTPBasic|Depends\(get_current_user\)|secrets\.[0-9A-Za-z_]+|hmac\.[0-9A-Za-z_]+|hashlib\.[0-9A-Za-z_]+|itsdangerous|serializer\.dumps|serializer\.loads|token_urlsafe|oauth|authlib|social_core|allauth)(?:\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+)?$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:passwordEncoder|bCryptPasswordEncoder|encoder|jwtUtil[0-9A-Za-z_]*|jwtService|jwtProvider|tokenProvider|tokenService|jwtDecoder|jwtEncoder|Jwts|JWT|Jwt|authenticationManager|authManager|SecurityContextHolder|securityContext|userDetailsService|BCrypt|Keys|Algorithm|Argon2[0-9A-Za-z_]*|SCrypt[0-9A-Za-z_]*|Pbkdf2[0-9A-Za-z_]*|MessageDigest|Mac|SecureRandom|keycloak|oauth2[0-9A-Za-z_]*|OAuth2[0-9A-Za-z_]*|clientRegistrationRepository|authorizedClientService)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:this\.)?_?(?:userManager|signInManager|roleManager|tokenHandler|jwtHandler|JwtSecurityTokenHandler|tokenService|jwtService|authService|authenticationService|passwordHasher|PasswordHasher|HttpContext\.SignInAsync|HttpContext\.SignOutAsync|HttpContext\.AuthenticateAsync|HttpContext\.ChallengeAsync|context\.SignInAsync|context\.SignOutAsync|identityService|_identityService|currentUser|_currentUser|user\.Identity|User\.Identity|User\.Claims|User\.IsInRole|BCrypt|Argon2|Rfc2898DeriveBytes|RandomNumberGenerator|SHA256|HMACSHA256|KeyDerivation|Convert\.ToBase64String)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$|^(?:JwtSecurityToken|JwtSecurityTokenHandler|SymmetricSecurityKey|SigningCredentials|ClaimsIdentity|ClaimsPrincipal|AuthenticationProperties)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:jwt|jose|paseto|bcrypt|argon2|scrypt|pbkdf2|hmac|sha256|oauth2|oidc|auth|authz|casbin|token|session|sessions|securecookie|gothic|goth)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$|^[0-9A-Za-z_]+\.(?:SignedString|ParseWithClaims|GenerateFromPassword|CompareHashAndPassword|Verify|VerifyToken|GenerateToken|ValidateToken|Authenticate|Authorize|Login|Logout|CheckPassword|HashPassword)$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:BCrypt::Password\.[0-9A-Za-z_]+|JWT\.(?:encode|decode)|sign_in|sign_out|sign_in_and_redirect|authenticate_user!|authenticate_with_http_token|authenticate_or_request_with_http_basic|current_user|has_secure_password|Devise\.[0-9A-Za-z_]+|warden\.[0-9A-Za-z_]+|OmniAuth\.[0-9A-Za-z_]+|Doorkeeper\.[0-9A-Za-z_]+|SecureRandom\.[0-9A-Za-z_]+|Digest::[0-9A-Za-z_]+\.[0-9A-Za-z_]+)$|^[0-9A-Za-z_]+\.(?:authenticate|authenticate!|regenerate_token|generate_token|valid_password\?)$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:Auth::[0-9A-Za-z_]+|Hash::[0-9A-Za-z_]+|Password::[0-9A-Za-z_]+|Crypt::[0-9A-Za-z_]+|Gate::[0-9A-Za-z_]+|Socialite::[0-9A-Za-z_]+|Passport::[0-9A-Za-z_]+|Sanctum::[0-9A-Za-z_]+|JWTAuth::[0-9A-Za-z_]+|password_hash|password_verify|\$request->user|\$user->createToken|\$user->tokens)(?:\(\)->[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:jsonwebtoken|bcrypt|argon2|scrypt|pbkdf2|hmac|sha2|ring|rustls|oauth2|openidconnect|jwt_simple|biscuit|paseto)(?:::[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Rs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Auth,
        test: Test::Pattern(
            r#"^(?:crypt|getpwnam|getpwuid|setuid|setgid|seteuid|PAM_[0-9A-Za-z_]+|pam_[0-9A-Za-z_]+|SSL_CTX_[0-9A-Za-z_]+|EVP_[0-9A-Za-z_]+|HMAC|RAND_bytes|BN_[0-9A-Za-z_]+|X509_[0-9A-Za-z_]+|gnutls_[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::C],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:AsyncStorage|SecureStore|MMKV|localStorage|sessionStorage|indexedDB|UserDefaults|Keychain|KeychainAccess|FileSystem|RNFS|FileManager|fs|fsp|fs\.promises|promises|path\.write|Deno\.(?:writeFile|readFile|writeTextFile|readTextFile|remove|mkdir|open|create|stat)|Bun\.(?:write|file))(?-u:\b)"#,
        ),
        only: &[],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:s3|s3Client|S3|storage|bucket|gcs|blob|blobClient|blobService|containerClient|cloudinary|uploader|uploadthing|utapi|supabase\.storage|firebase\.storage|storageRef|ref|getStorage|minio|minioClient|r2|R2|env\.[0-9A-Za-z_]*(?:BUCKET|R2)[0-9A-Za-z_]*|sharp|multer)\.(?:[0-9A-Za-z_]+\.)*(?:putObject|getObject|deleteObject|listObjects[0-9A-Za-z_]*|upload[0-9A-Za-z_]*|download[0-9A-Za-z_]*|send|file|save|delete|remove|getSignedUrl|createSignedUrl|createReadStream|createWriteStream|put|get|head|copy|move|exists|list|write[0-9A-Za-z_]*|read[0-9A-Za-z_]*|toFile|toBuffer|from|upload_stream|destroy|createPresignedPost|presign[0-9A-Za-z_]*|bucket|object)$|^(?:PutObjectCommand|GetObjectCommand|DeleteObjectCommand|CopyObjectCommand|HeadObjectCommand|ListObjectsV2Command|CreateMultipartUploadCommand|UploadPartCommand|Upload)$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:open|os\.(?:remove|unlink|rename|replace|makedirs|mkdir|rmdir|removedirs|listdir|scandir|walk|chmod|chown|stat|path\.exists|path\.isfile|path\.isdir|path\.getsize|symlink|link|truncate|utime|fsync)|shutil\.[0-9A-Za-z_]+|tempfile\.[0-9A-Za-z_]+|Path\([0-9A-Za-z_]*\)\.(?:write_text|write_bytes|read_text|read_bytes|unlink|mkdir|rmdir|rename|replace|touch|exists|iterdir|glob|rglob|open)|[0-9A-Za-z_]+\.(?:write_text|write_bytes|read_text|read_bytes|unlink|mkdir|rmdir|touch)|boto3\.(?:client|resource)|s3\.(?:upload_file|upload_fileobj|download_file|download_fileobj|put_object|get_object|delete_object|list_objects[0-9A-Za-z_]*|head_object|copy_object|generate_presigned_url|create_bucket)|s3_client\.[0-9A-Za-z_]+|bucket\.(?:upload_file|download_file|put_object|delete_objects|objects)|default_storage\.[0-9A-Za-z_]+|storage\.[0-9A-Za-z_]+|FileSystemStorage|blob\.(?:upload_from_[0-9A-Za-z_]+|download_as_[0-9A-Za-z_]+|download_to_[0-9A-Za-z_]+|delete|exists)|bucket\.blob|gcs\.[0-9A-Za-z_]+|blob_client\.[0-9A-Za-z_]+|container_client\.[0-9A-Za-z_]+|json\.dump|pickle\.dump|pickle\.load|json\.load|csv\.writer|csv\.reader|zipfile\.ZipFile|tarfile\.open|gzip\.open|aiofiles\.open|anyio\.open_file|shelve\.open|sqlite3\.connect|dbm\.open)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[dD]ataStore|[0-9A-Za-z_]*[pP]references|[0-9A-Za-z_]*[pP]refs|sharedPreferences|prefs|editor)\.(?:updateData|edit|data|apply|commit|getString|putString|getInt|putInt|getLong|putLong|getBoolean|putBoolean|getFloat|putFloat|getStringSet|putStringSet|remove|clear|contains)$|^(?:this\.)?(?:context|applicationContext|appContext|ctx)\.(?:openFileOutput|openFileInput|getSharedPreferences|deleteFile|getFilesDir|getCacheDir|getExternalFilesDir|getDatabasePath|deleteDatabase|contentResolver\.[0-9A-Za-z_]+)$|^(?:this\.)?(?:contentResolver|resolver)\.(?:query|insert|update|delete|openInputStream|openOutputStream)$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Queue,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:[0-9A-Za-z_]*[wW]orkManager|[0-9A-Za-z_]*[sS]cheduler|jobScheduler|alarmManager|[0-9A-Za-z_]*AlarmManager)\.(?:enqueue[0-9A-Za-z_]*|beginWith|beginUniqueWork|cancel[0-9A-Za-z_]*|schedule[0-9A-Za-z_]*|set[0-9A-Za-z_]*|setExact[0-9A-Za-z_]*|setRepeating|setInexactRepeating)$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:Files|Paths|File|FileUtils|IOUtils|FileSystems|Channels|FileChannel)\.[0-9A-Za-z_]+$|^(?:this\.)?(?:s3Client|amazonS3|s3|storage|gcsStorage|blobClient|blobServiceClient|containerClient|minioClient|storageService|fileService|fileStorage|resourceLoader|resource)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$|^(?:FileInputStream|FileOutputStream|FileReader|FileWriter|RandomAccessFile|BufferedWriter|BufferedReader|PrintWriter|PutObjectRequest|GetObjectRequest|DeleteObjectRequest|ObjectMetadata)$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:File|Directory|Path\.(?:Combine)?|FileInfo|DirectoryInfo|FileStream|StreamWriter|StreamReader|BinaryWriter|BinaryReader|ZipFile|ZipArchive|IsolatedStorageFile)(?:\.[0-9A-Za-z_]+)*$|^(?:this\.)?_?(?:s3Client|blobClient|blobServiceClient|containerClient|blobContainer|storage|storageService|fileStorage|fileService|fileProvider|_fileProvider|amazonS3|minio|minioClient)\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+$|^(?:FileStream|StreamWriter|StreamReader|BinaryWriter|BinaryReader|PutObjectRequest|GetObjectRequest|DeleteObjectRequest|TransferUtility)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:os|ioutil|io|filepath|bufio|afero|fs)\.(?:Open|OpenFile|Create|CreateTemp|ReadFile|WriteFile|Remove|RemoveAll|MkdirAll|Mkdir|MkdirTemp|Rename|Stat|Lstat|ReadDir|Chmod|Chown|Truncate|Symlink|Link|Readlink|Copy|CopyN|WriteString|Walk|WalkDir|Glob|NewWriter|NewReader|TempDir|TempFile|ReadAll)$|^(?:f|file|fd|w|writer)\.(?:Write[0-9A-Za-z_]*|Read[0-9A-Za-z_]*|Close|Sync|Seek|Truncate|WriteString)$|^(?:s3|s3Client|svc|uploader|downloader|storage|bucket|client|minioClient|blob)\.(?:PutObject[0-9A-Za-z_]*|GetObject[0-9A-Za-z_]*|DeleteObject[0-9A-Za-z_]*|ListObjects[0-9A-Za-z_]*|HeadObject[0-9A-Za-z_]*|CopyObject[0-9A-Za-z_]*|Upload[0-9A-Za-z_]*|Download[0-9A-Za-z_]*|Object|Bucket|Write|NewWriter|NewReader|FPutObject|FGetObject|PresignedGetObject|PresignedPutObject)$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:fopen|freopen|fclose|fread|fwrite|fgets|fputs|fgetc|fputc|fscanf|fprintf|fflush|fseek|ftell|rewind|open|openat|creat|close|read|write|pread|pwrite|lseek|unlink|unlinkat|remove|rename|renameat|mkdir|mkdirat|rmdir|stat|fstat|lstat|fstatat|access|chmod|fchmod|chown|fchown|truncate|ftruncate|fsync|fdatasync|opendir|fdopendir|readdir|closedir|rewinddir|mmap|munmap|msync|flock|fcntl|dup|dup2|pipe|mkstemp|mkdtemp|tmpfile|realpath|readlink|symlink|link|utime|utimes|futimes|sendfile|copy_file_range|ioctl|CreateFile[0-9A-Za-z_]*|ReadFile|WriteFile|CloseHandle|DeleteFile[0-9A-Za-z_]*|MoveFile[0-9A-Za-z_]*|CopyFile[0-9A-Za-z_]*|CreateDirectory[0-9A-Za-z_]*|RemoveDirectory[0-9A-Za-z_]*|FindFirstFile[0-9A-Za-z_]*|FindNextFile[0-9A-Za-z_]*|GetFileAttributes[0-9A-Za-z_]*|SetFilePointer[0-9A-Za-z_]*|FlushFileBuffers|std::ofstream|std::ifstream|std::fstream|ofstream|ifstream|fstream|std::filesystem::[0-9A-Za-z_]+|filesystem::[0-9A-Za-z_]+|fs::[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::C],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:File|Dir|FileUtils|IO|Pathname|Tempfile)\.[0-9A-Za-z_]+$|^(?:Aws::S3::[0-9A-Za-z_]+|S3_BUCKET\.[0-9A-Za-z_]+|ActiveStorage::[0-9A-Za-z_]+|[0-9A-Za-z_]+\.attach|[0-9A-Za-z_]+\.purge|[0-9A-Za-z_]+\.purge_later)$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:Storage::[0-9A-Za-z_]+|File::[0-9A-Za-z_]+|file_put_contents|file_get_contents|fopen|fwrite|fread|fclose|unlink|mkdir|rmdir|rename|copy|move_uploaded_file|\$request->file|\$file->store[0-9A-Za-z_]*|\$file->move)(?:\(\)->[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Storage,
        test: Test::Pattern(
            r#"^(?:std::fs|fs|tokio::fs|File|OpenOptions|std::io::Write|BufWriter|BufReader)(?:::[0-9A-Za-z_]+)*$|^[0-9A-Za-z_]+\.(?:write_all|read_to_string|read_to_end|sync_all|flush)$"#,
        ),
        only: &[Family::Rs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:fetch|axios|ky|got|superagent|XMLHttpRequest|WebSocket|EventSource|undici|request|needle|phin|ofetch|\$fetch|useFetch|useAsyncData|wretch|redaxios)$|^(?:this\.)?(?:axios|api|client|http|https|httpClient|apiClient|instance|request|agent|graphql|apollo|apolloClient|urql|trpc|supabase|octokit|github|gh|openai|anthropic|ai|slack|twilio|sdk|[0-9A-Za-z_]+Client|[0-9A-Za-z_]+Api|[0-9A-Za-z_]+API|[0-9A-Za-z_]+Sdk|[0-9A-Za-z_]+SDK|\$http|\$axios|\$api|api\.[0-9A-Za-z_]+|routes\.[0-9A-Za-z_]+|server\.[0-9A-Za-z_]+)\.(?:[0-9A-Za-z_]+\.)*(?:get|post|put|patch|delete|head|options|request|query|mutate|mutation|rpc|invoke|call|send|fetch|create|list|retrieve|update|del|remove|upload|download|stream|connect|subscribe|emit|generate|complete|chat\.completions\.create|messages\.create|embeddings\.create|images\.generate|run|search|execute|useQuery|useMutation|useInfiniteQuery|prefetchQuery|fetchQuery|ensureQueryData)$|^URLSession(?:\.|$)|^(?:Alamofire|AF)\.|\.(?:dataTask|uploadTask|downloadTask)$|^(?:io|socket|ws|wss|pusher|ably|centrifuge|mqtt|mqttClient|nc|nats|grpc|grpcClient|stub)\.(?:emit|send|publish|connect|request|call|to|in|of|subscribe|unsubscribe|invoke)$|^(?:io|WebSocket|EventSource|XMLHttpRequest|Pusher|Ably|Centrifuge)$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:requests|httpx|aiohttp|urllib\.request|urllib3|http\.client|httplib2|treq|niquests|pycurl|websockets|websocket|socketio|sio|grpc|stub|channel|zmq|paho|mqtt|client|http_client|api_client|api|session_client|async_client|_client|self\.client|self\.http|self\.session_client|slack_client|openai|anthropic|boto3\.client|lambda_client|sqs_client|sns_client|ses_client|ec2|s3)\.(?:[0-9A-Za-z_]+\.)*(?:get|post|put|patch|delete|head|options|request|send|fetch|urlopen|open|ClientSession|AsyncClient|Client|Session|stream|connect|emit|invoke|call|create|chat\.completions\.create|messages\.create|completions\.create|embeddings\.create|generate|retrieve|list|update|search|query)$|^(?:urlopen|Request|websockets\.connect|aiohttp\.ClientSession|httpx\.AsyncClient|httpx\.Client|requests\.Session|socket\.socket|socket\.create_connection|grpc\.insecure_channel|grpc\.secure_channel|grpc\.aio\.insecure_channel)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:this\.)?(?:restTemplate|webClient|httpClient|client|okHttpClient|retrofit|feignClient|[0-9A-Za-z_]*[cC]lient|[0-9A-Za-z_]*Feign|[0-9A-Za-z_]*Api|[0-9A-Za-z_]*Stub|[0-9A-Za-z_]*BlockingStub|[0-9A-Za-z_]*AsyncStub|graphQlClient|restClient|RestClient|WebClient|HttpClient|HttpRequest|Unirest|Jsoup|template)\.(?:[0-9A-Za-z_]+\.)*(?:getForObject|getForEntity|postForObject|postForEntity|postForLocation|exchange|execute|put|delete|patchForObject|get|post|patch|head|options|send|sendAsync|newCall|retrieve|bodyToMono|bodyToFlux|create|builder|newBuilder|newHttpClient|connect|call|invoke|newRequest|uri|method|request|body|block|subscribe|fetch|execute[0-9A-Za-z_]*|list[0-9A-Za-z_]*|get[0-9A-Za-z_]+|create[0-9A-Za-z_]+|update[0-9A-Za-z_]+|delete[0-9A-Za-z_]+)$|^(?:Socket|ServerSocket|URL|HttpURLConnection|HttpsURLConnection|DatagramSocket|WebSocketClient|StompSession|ManagedChannelBuilder)$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:this\.)?_?(?:httpClient|client|http|httpClientFactory|[0-9A-Za-z_]*[cC]lient|[0-9A-Za-z_]*Api|graphQLClient|restClient|flurl|[0-9A-Za-z_]+\.WithOAuthBearerToken)\.(?:[0-9A-Za-z_]+\.)*(?:GetAsync|PostAsync|PutAsync|PatchAsync|DeleteAsync|SendAsync|GetStringAsync|GetStreamAsync|GetByteArrayAsync|GetFromJsonAsync|PostAsJsonAsync|PutAsJsonAsync|PatchAsJsonAsync|DeleteFromJsonAsync|Send|CreateClient|GetJsonAsync|PostJsonAsync|ReceiveJson|ReceiveString|InvokeAsync|SendCoreAsync|StartAsync|ConnectAsync|Get[0-9A-Za-z_]+Async|Post[0-9A-Za-z_]+Async|Put[0-9A-Za-z_]+Async|Delete[0-9A-Za-z_]+Async|List[0-9A-Za-z_]+Async|Create[0-9A-Za-z_]+Async|Update[0-9A-Za-z_]+Async|Invoke[0-9A-Za-z_]+Async|Execute[0-9A-Za-z_]*Async)$|^(?:HttpClient|HttpRequestMessage|WebClient|HttpWebRequest|TcpClient|UdpClient|Socket|ClientWebSocket|HubConnection|HubConnectionBuilder|GrpcChannel|RestClient|RestRequest|FlurlClient)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:http|client|httpClient|c|hc|resty|req|grpc|net|websocket|ws|conn|nc|nats|mqtt|redis|rdb|[0-9A-Za-z_]+Client|[0-9A-Za-z_]+client|svc|api|sdk)\.(?:[0-9A-Za-z_]+\.)*(?:Get|Post|PostForm|Put|Patch|Delete|Head|Do|NewRequest|NewRequestWithContext|R|Dial|DialContext|DialTLS|Listen|ListenAndServe|Invoke|NewStream|Connect|Send|Recv|Request|Call|Write[0-9A-Za-z_]*|Read[0-9A-Za-z_]*|Publish|Subscribe|SendMessage|Ping|Execute|Fetch|Query|Mutate|Invoke[0-9A-Za-z_]*|Get[0-9A-Za-z_]+|Post[0-9A-Za-z_]+|Put[0-9A-Za-z_]+|Delete[0-9A-Za-z_]+|List[0-9A-Za-z_]+|Create[0-9A-Za-z_]+|Update[0-9A-Za-z_]+|Describe[0-9A-Za-z_]+)$|^(?:http|net|grpc|websocket|resty|fasthttp|gorequest|req)\.(?:Get|Post|PostForm|Head|Dial|DialContext|Listen|ListenAndServe|ListenAndServeTLS|NewClient|NewRequest|Serve|DefaultDialer\.Dial|Upgrade)$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:socket|connect|bind|listen|accept|accept4|send|sendto|sendmsg|recv|recvfrom|recvmsg|getaddrinfo|gethostbyname|getnameinfo|inet_pton|inet_ntop|setsockopt|getsockopt|shutdown|select|poll|epoll_create[0-9A-Za-z_]*|epoll_ctl|epoll_wait|kqueue|kevent|curl_easy_init|curl_easy_setopt|curl_easy_perform|curl_easy_cleanup|curl_multi_[0-9A-Za-z_]+|SSL_new|SSL_connect|SSL_accept|SSL_read|SSL_write|SSL_shutdown|SSL_free|BIO_[0-9A-Za-z_]+|WSAStartup|WSASocket[0-9A-Za-z_]*|WSASend|WSARecv|WSACleanup|closesocket|ioctlsocket|http_[0-9A-Za-z_]+|uv_tcp_[0-9A-Za-z_]+|uv_udp_[0-9A-Za-z_]+|uv_connect|uv_listen|uv_read_start|uv_write|evhttp_[0-9A-Za-z_]+|bufferevent_[0-9A-Za-z_]+|nng_[0-9A-Za-z_]+|zmq_[0-9A-Za-z_]+|MHD_[0-9A-Za-z_]+|mg_[0-9A-Za-z_]+|lws_[0-9A-Za-z_]+|ares_[0-9A-Za-z_]+|anetTcpConnect|anetTcpServer|anetAccept|anetRead|anetWrite|connSocket[0-9A-Za-z_]*|connConnect|connWrite|connRead|connAccept|connListen|aeCreateFileEvent|aeDeleteFileEvent)$"#,
        ),
        only: &[Family::C],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:Net::HTTP(?:\.[0-9A-Za-z_]+)*|HTTParty\.[0-9A-Za-z_]+|Faraday(?:\.[0-9A-Za-z_]+)*|RestClient\.[0-9A-Za-z_]+|HTTP\.[0-9A-Za-z_]+|Excon\.[0-9A-Za-z_]+|Typhoeus\.[0-9A-Za-z_]+|OpenURI\.open_uri|URI\.open|open-uri|Socket\.[0-9A-Za-z_]+|TCPSocket\.[0-9A-Za-z_]+|WebSocket::[0-9A-Za-z_]+|ActionCable\.server\.broadcast|[0-9A-Za-z_]+Channel\.broadcast_to|[0-9A-Za-z_]+Channel\.broadcast|[0-9A-Za-z_]+\.broadcast)$|^[0-9A-Za-z_]+\.(?:get|post|put|patch|delete|head|request)$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:Http::[0-9A-Za-z_]+|Http::[0-9A-Za-z_]+::[0-9A-Za-z_]+|curl_init|curl_exec|curl_setopt[0-9A-Za-z_]*|curl_close|file_get_contents|fsockopen|stream_socket_client|socket_create|socket_connect|socket_send|socket_recv|\$client->(?:request|get|post|put|patch|delete|send|sendAsync|requestAsync)|\$guzzle->[0-9A-Za-z_]+|\$http->[0-9A-Za-z_]+)(?:\(\)->[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^(?:reqwest|hyper|ureq|isahc|surf|tonic|tungstenite|tokio_tungstenite|websocket|TcpStream|TcpListener|UdpSocket|Client|ClientBuilder|Request|awc)(?:::[0-9A-Za-z_]+)*$|^[0-9A-Za-z_]+\.(?:get|post|put|patch|delete|head|send|execute|connect|bind|send_to|recv_from|write_all|read_to_end)$"#,
        ),
        only: &[Family::Rs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Network,
        test: Test::Pattern(
            r#"^URLSession(?:\.|$)|^(?:Alamofire|AF)\.|\.(?:dataTask|uploadTask|downloadTask|webSocketTask|responseDecodable|responseJSON|responseData)$|(?:^|\.)[0-9A-Za-z_]*[sS]ession\.(?:data|upload|download|bytes)$|^(?:NWConnection|NWListener|NWBrowser|URLSessionWebSocketTask|WebSocket|Starscream|SocketManager|SocketIOClient|Socket)(?-u:\b)|^(?:this\.|self\.)?(?:client|api|apiClient|http|httpClient|networkService|network|session)\.(?:get|post|put|patch|delete|request|send|fetch|perform|execute|call|data|upload|download)$"#,
        ),
        only: &[Family::Swift],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Device,
        test: Test::Pattern(
            r#"^(?:Linking|Share|Clipboard|Notifications|Camera|ImagePicker|MediaLibrary|Haptics|Alert|Vibration|Location|Geolocation|Permissions|UIApplication|AVCaptureSession|AVAudioSession|CLLocationManager|UNUserNotificationCenter|Battery|Brightness|Sensors|Accelerometer|Gyroscope|Magnetometer|Pedometer|Contacts|Calendar|LocalAuthentication|BiometricAuth|DocumentPicker|Print|ScreenOrientation|StatusBar|BackHandler|Appearance|Dimensions|PixelRatio|Keyboard|PushNotification|PushNotificationIOS|messaging|Bluetooth|BleManager|NfcManager|navigator\.[0-9A-Za-z_]+|window\.(?:open|print|alert|confirm|prompt)|Notification|speechSynthesis|WebAuthn|Intent|intent|context\.startActivity|startActivity|startService|sendBroadcast|registerReceiver|NotificationManager|notificationManager|NotificationCompat|LocationManager|locationManager|fusedLocationClient|SensorManager|sensorManager|CameraX|cameraProvider|MediaPlayer|mediaPlayer|AudioManager|audioManager|Vibrator|vibrator|ClipboardManager|clipboardManager|UIDevice|UIPasteboard|UIImpactFeedbackGenerator|UINotificationFeedbackGenerator|AVAudioPlayer|AVPlayer|CMMotionManager|PHPhotoLibrary|UIImagePickerController|LAContext|WKWebView|Process\.Start|Environment\.Exit|Clipboard\.[0-9A-Za-z_]+|Console\.[0-9A-Za-z_]+)(?-u:\b)"#,
        ),
        only: &[],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Telemetry,
        test: Test::Pattern(
            r#"^(?:DdRum|DdLogs|DdTrace|DdSdkReactNative|CustomerIO|Sentry|Bugsnag|analytics|Analytics|crashlytics|Crashlytics|mixpanel|Mixpanel|amplitude|Amplitude|posthog|PostHog|LDClient|ldClient|Datadog|datadog|datadogRum|datadogLogs|newrelic|NewRelic|honeycomb|Honeycomb|segment|Segment|statsd|StatsD|metrics|Metrics|meter|Meter|meterRegistry|MeterRegistry|counter|histogram|tracer|Tracer|otel|opentelemetry|trace\.getTracer|span|Span|appInsights|TelemetryClient|_telemetryClient|telemetryClient|telemetry|_telemetry|Telemetry|Application\.Insights|logtail|Logtail|rollbar|Rollbar|raven|Raven|prometheus|Prometheus|promClient|prom|registry|Registry|sentry_sdk|capture_exception|capture_message|statsd_client|dogstatsd|MetricRegistry|Micrometer|Timer|Counter|Gauge|Histogram|Summary)(?-u:\b)(?:\.(?:[0-9A-Za-z_]+\.)*[0-9A-Za-z_]+)?$"#,
        ),
        only: &[],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:child_process|spawn|spawnSync|exec|execSync|execFile|execFileSync|fork|process\.exit|process\.kill|process\.abort|Deno\.(?:run|exit|Command|kill)|Bun\.(?:spawn|spawnSync|\$)|\$`|execa|execaSync|\$|zx|Worker|worker_threads|cluster\.fork|os\.setPriority|pm2\.[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::Js],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:subprocess\.(?:run|call|check_call|check_output|Popen|getoutput|getstatusoutput)|os\.(?:system|popen|execv|execve|execvp|execl|execlp|spawn[0-9A-Za-z_]*|fork|forkpty|kill|killpg|_exit|abort|nice|setsid|setuid|setgid|waitpid|wait)|sys\.exit|exit|quit|multiprocessing\.(?:Process|Pool)|Process|Pool|signal\.signal|signal\.alarm|pty\.spawn|asyncio\.create_subprocess_[0-9A-Za-z_]+|sh\.[0-9A-Za-z_]+|plumbum\.[0-9A-Za-z_]+|pexpect\.[0-9A-Za-z_]+|importlib\.import_module|__import__|ctypes\.[0-9A-Za-z_]+|cffi\.[0-9A-Za-z_]+)$"#,
        ),
        only: &[Family::Py],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:Runtime\.getRuntime\(\)\.exec|Runtime\.getRuntime\(\)\.halt|Runtime\.getRuntime|runtime\.exec|ProcessBuilder|System\.exit|System\.loadLibrary|System\.load|Thread\.sleep|Thread|Executors\.[0-9A-Za-z_]+|executor\.[0-9A-Za-z_]+|ForkJoinPool\.[0-9A-Za-z_]+|CompletableFuture\.[0-9A-Za-z_]+|Runtime\.exit|Runtime\.halt|exitProcess|ProcessHandle\.[0-9A-Za-z_]+|Signal\.[0-9A-Za-z_]+|thread|Timer|ScheduledExecutorService)(?:\.[0-9A-Za-z_]+)*$"#,
        ),
        only: &[Family::Jvm],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:Process\.(?:Start|Kill|GetProcesses[0-9A-Za-z_]*|GetCurrentProcess)|Environment\.(?:Exit|FailFast)|AppDomain\.[0-9A-Za-z_]+|Thread\.(?:Sleep|Start)|Task\.(?:Run|Factory\.StartNew|Delay)|ThreadPool\.[0-9A-Za-z_]+|Assembly\.(?:Load[0-9A-Za-z_]*)|Activator\.CreateInstance[0-9A-Za-z_]*|Marshal\.[0-9A-Za-z_]+|NativeLibrary\.[0-9A-Za-z_]+|ProcessStartInfo)$"#,
        ),
        only: &[Family::Cs],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:exec\.(?:Command|CommandContext|LookPath)|os\.(?:Exit|StartProcess|FindProcess|Getpid|Getenv|Setenv|Executable)|syscall\.[0-9A-Za-z_]+|signal\.(?:Notify|NotifyContext|Stop|Ignore|Reset)|log\.(?:Fatal[0-9A-Za-z_]*|Panic[0-9A-Za-z_]*)|runtime\.(?:GC|Goexit|GOMAXPROCS)|plugin\.Open|debug\.SetGCPercent|cmd\.(?:Run|Start|Output|CombinedOutput|Wait|Kill|StdoutPipe|StdinPipe|StderrPipe))$"#,
        ),
        only: &[Family::Go],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:fork|vfork|clone|execv|execve|execvp|execvpe|execl|execle|execlp|posix_spawn[0-9A-Za-z_]*|system|popen|pclose|waitpid|wait|wait3|wait4|waitid|kill|killpg|raise|signal|sigaction|sigprocmask|sigsuspend|sigwait|alarm|setitimer|pause|exit|_exit|_Exit|abort|atexit|quick_exit|setsid|setpgid|setpgrp|getpid|getppid|daemon|nice|setpriority|setrlimit|getrlimit|chroot|setuid|setgid|seteuid|setegid|setgroups|pthread_create|pthread_join|pthread_cancel|pthread_kill|pthread_detach|pthread_exit|thrd_create|thrd_join|dlopen|dlsym|dlclose|dlerror|LoadLibrary[0-9A-Za-z_]*|GetProcAddress|FreeLibrary|CreateProcess[0-9A-Za-z_]*|CreateThread|ExitProcess|ExitThread|TerminateProcess|TerminateThread|ShellExecute[0-9A-Za-z_]*|WinExec|WaitForSingleObject|WaitForMultipleObjects|sched_yield|sched_setaffinity|prctl|ptrace|uv_spawn|uv_process_kill|redisFork|bioCreateBackgroundJob|bioSubmitJob)$"#,
        ),
        only: &[Family::C],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:system|spawn|exec|fork|Process\.[0-9A-Za-z_]+|Open3\.[0-9A-Za-z_]+|Kernel\.(?:system|spawn|exec|exit|exit!|abort|at_exit)|exit|exit!|abort|at_exit|Signal\.trap|trap|Thread\.new|Thread\.start|IO\.popen|PTY\.spawn|`)$"#,
        ),
        only: &[Family::Rb],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:exec|shell_exec|system|passthru|proc_open|popen|pcntl_[0-9A-Za-z_]+|posix_[0-9A-Za-z_]+|exit|die|Process::[0-9A-Za-z_]+|Artisan::[0-9A-Za-z_]+|new Process|Process)$"#,
        ),
        only: &[Family::Php],
        instantiates: false,
    },
    EffectRule {
        category: EffectCategory::Process,
        test: Test::Pattern(
            r#"^(?:std::process|process|Command|std::thread|thread|tokio::spawn|tokio::process|spawn|rayon|libc)(?:::[0-9A-Za-z_]+)*$|^[0-9A-Za-z_]+\.(?:spawn|output|status|wait|kill)$"#,
        ),
        only: &[Family::Rs],
        instantiates: false,
    },
];
const EXCEPTION_RESPONSE: &str = r#"(?:^|[.:\\])(?:[0-9A-Za-z_]+Exception|[0-9A-Za-z_]*HttpError|ApiError|[0-9A-Za-z_]+ApiError|HttpProblem|ProblemDetails|Abort|ResponseStatusException|ErrorResponse|[0-9A-Za-z_]+ErrorResponse|HTTPError|HTTPException|APIException|Http[0-9]{3}|[0-9A-Za-z_]*(?:NotFound|BadRequest|Unauthorized|Unauthenticated|Forbidden|Conflict|Validation|Unprocessable|TooManyRequests|Gone|NotAllowed|MethodNotAllowed|Unsupported|RequestTimeout|InternalServer|ServiceUnavailable|PaymentRequired|PreconditionFailed|NotAcceptable|NotImplemented|BadGateway|RateLimit)Error)$"#;
const NOT_A_RESPONSE: &str = r#"^(?:Error|TypeError|RangeError|SyntaxError|ReferenceError|EvalError|URIError|AggregateError|Exception|RuntimeException|IllegalArgumentException|IllegalStateException|NullPointerException|IndexOutOfBoundsException|UnsupportedOperationException|ArgumentException|ArgumentNullException|ArgumentOutOfRangeException|InvalidOperationException|NotImplementedException|NotSupportedException|ValueError|TypeError|KeyError|IndexError|RuntimeError|NotImplementedError|AssertionError|StopIteration|InterruptedException|IOException|FileNotFoundException|ClassNotFoundException|NoSuchElementException|NumberFormatException|CloneNotSupportedException|ExecutionException|TimeoutException|OperationCanceledException|TaskCanceledException|ObjectDisposedException|FormatException|OverflowException|DivideByZeroException|JsonException|SerializationException|ParseException|DateTimeParseException|MalformedURLException|URISyntaxException|SQLException|DataAccessException|DbUpdateException|ConcurrencyException|EntityNotFoundException|NoResultException|OptimisticLockException|InvalidArgumentException|LogicException|DomainException|LengthException|OutOfRangeException|OutOfBoundsException|UnexpectedValueException|BadMethodCallException|BadFunctionCallException|RangeException|UnderflowException|ErrorException|JsonException|TypeError)$"#;
const READ_OPS: &str = r#"(?i)^(?:find[0-9A-Za-z_]*|findone|get|getone|getmany|getall[0-9A-Za-z_]*|getbyid[0-9A-Za-z_]*|getasync|list[0-9A-Za-z_]*|count[0-9A-Za-z_]*|aggregate|select[0-9A-Za-z_]*|query[0-9A-Za-z_]*|queryrow[0-9A-Za-z_]*|scalar[0-9A-Za-z_]*|first[0-9A-Za-z_]*|single[0-9A-Za-z_]*|last[0-9A-Za-z_]*|all|any[0-9A-Za-z_]*|exists[0-9A-Za-z_]*|tolist[0-9A-Za-z_]*|toarray[0-9A-Za-z_]*|filter|where|fetch[0-9A-Za-z_]*|load[0-9A-Za-z_]*|read[0-9A-Za-z_]*|scan|search|paginate|pluck|distinct|group[0-9A-Za-z_]*|order[0-9A-Za-z_]*|include[0-9A-Za-z_]*|join[0-9A-Za-z_]*|objects|values|values_list|iterator|stream|max|min|sum|avg|average|exists\?|one|one_or_none|scalars?|find_by|find_each|firstordefault[0-9A-Za-z_]*|singleordefault[0-9A-Za-z_]*|countdocuments|rows|row|preload|take|limit|offset|skip|raw|fromsql[0-9A-Za-z_]*|\$queryraw|get_or_404|get_object_or_404|describe[0-9A-Za-z_]*|head[0-9A-Za-z_]*|retrieve|show|index|lookup|peek|contains|has|check|isempty|is_empty|size|length|fetchone|fetchall|fetchmany|refresh)$"#;
const WRITE_OPS: &str = r#"(?i)^(?:create[0-9A-Za-z_]*|update[0-9A-Za-z_]*|upsert[0-9A-Za-z_]*|delete[0-9A-Za-z_]*|destroy[0-9A-Za-z_]*|save[0-9A-Za-z_]*|insert[0-9A-Za-z_]*|remove[0-9A-Za-z_]*|add[0-9A-Za-z_]*|persist[0-9A-Za-z_]*|merge|flush|commit|bulk[0-9A-Za-z_]*|batch[0-9A-Za-z_]*|put[0-9A-Za-z_]*|set[0-9A-Za-z_]*|exec|execute[0-9A-Za-z_]*|savechanges[0-9A-Za-z_]*|increment[0-9A-Za-z_]*|decrement[0-9A-Za-z_]*|truncate|drop|sqlmodel_update|find_or_create[0-9A-Za-z_]*|update_or_create|firstorcreate|updateorcreate|get_or_create|delete_all|update_all|insert_all|upsert_all|refresh_from_db|patch|write[0-9A-Za-z_]*|push|pull|inc|attach|detach|sync|replace[0-9A-Za-z_]*|touch|purge|restore|forcedelete|store|migrate|automigrate|createmany|updatemany|deletemany|\$executeraw|\$transaction|transaction|begin|rollback|expunge|lock|unlock|import|reindex|rebuild|clear|reset|empty|fill|assign|append|prepend|move|copy|rename|link|unlink|register|unregister|enable|disable|activate|deactivate|approve|reject|publish|unpublish|archive|unarchive|complete|cancel|confirm|revoke|grant|deny|ban|unban|verify|invalidate|expire|evict|generate|seed)$"#;
const BUILTIN_RECEIVERS: &str = r#"^(?:Object|Array|Promise|Date|Map|Set|WeakMap|WeakSet|Reflect|Buffer|JSON|Math|Number|String|Symbol|Error|BigInt|Intl|Atomics|Proxy|Function|Boolean|RegExp|globalThis|window|document|console|process|List|Dict|Optional|Collections|Arrays|Objects|Stream|Task|Enumerable|Convert|Guid|DateTime|TimeSpan|Path|Regex)$"#;
const MODEL_SUFFIX: &str =
    r#"(?:Repository|Repo|Dao|DAO|Model|Mapper|Store|Collection|Table|Service|Entity|Manager)$"#;
const SWIFT_NOT_AN_EFFECT: &str = r#"^(?:Timer|Calendar|Intent|intent)(?-u:\b)"#;
const ORM_STATIC_OPS: &str = r#"(?:find|findOne|findAll|findById|findByPk|findMany|findFirst|findUnique|findOrCreate|findAndCountAll|create|createMany|update|updateOne|updateMany|upsert|delete|deleteOne|deleteMany|destroy|save|insert|insertMany|aggregate|count|countDocuments|bulkCreate|bulkWrite|exists|distinct|where|query|all|first|last|pluck|find_by|find_each|find_or_create_by|create!|update!|destroy_all|delete_all|update_all|insert_all|upsert|includes|joins|order|limit|scope|select)"#;
const DB_OP: &str = r#"(?:find[0-9A-Za-z_]*|get|getOne|getMany|getAll|getById|count[0-9A-Za-z_]*|aggregate|create[0-9A-Za-z_]*|update[0-9A-Za-z_]*|upsert|delete[0-9A-Za-z_]*|remove[0-9A-Za-z_]*|save[0-9A-Za-z_]*|insert[0-9A-Za-z_]*|exists|query[0-9A-Za-z_]*|execute[0-9A-Za-z_]*|exec|raw|persist|merge|flush|first[0-9A-Za-z_]*|last[0-9A-Za-z_]*|all|any[0-9A-Za-z_]*|toList[0-9A-Za-z_]*|toArray[0-9A-Za-z_]*|select[0-9A-Za-z_]*|where|orderBy|distinct|paginate|truncate|drop|Add[0-9A-Za-z_]*|Remove[0-9A-Za-z_]*|Update[0-9A-Za-z_]*|Find[0-9A-Za-z_]*|First[0-9A-Za-z_]*|Single[0-9A-Za-z_]*|ToList[0-9A-Za-z_]*|Count[0-9A-Za-z_]*|Any[0-9A-Za-z_]*|Create|Save|Delete|Where|Get|Select|Query[0-9A-Za-z_]*|Exec[0-9A-Za-z_]*|InsertOne|InsertMany|FindOne|UpdateOne|DeleteOne|DeleteMany|ReplaceOne|CountDocuments|bulk[0-9A-Za-z_]*|batch[0-9A-Za-z_]*)"#;
/// Upstream `RECEIVER_TYPE_RULES` (`effects.ts:229-238`).
const RECEIVER_TYPE_RULES: &[(EffectCategory, &str)] = &[
    (
        EffectCategory::Database,
        r#"(?:Repository|Repo|Dao|DAO|Mapper|EntityManager|DataSource|DbContext|DbSet|SessionFactory|JdbcTemplate|NamedParameterJdbcTemplate|MongoTemplate|MongoOperations|R2dbcEntityTemplate|DatabaseClient|PrismaClient|PrismaService|Knex|Kysely|Drizzle|Sequelize|ConnectionPool|MongoCollection|Datastore|IDbConnection|IRepository|IReadRepository|IUnitOfWork|UnitOfWork|DSLContext|EntityManagerFactory|SessionFactory|AsyncSession)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Network,
        r#"(?:HttpClient|RestTemplate|WebClient|RestClient|OkHttpClient|Retrofit|AxiosInstance|Axios|IHttpClientFactory|HttpClientFactory|GraphQLClient|GraphQlClient|WebSocketClient|StompSession|ManagedChannel|BlockingStub|AsyncStub|FeignClient|ApolloClient)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Auth,
        r#"(?:JwtService|JwtDecoder|JwtEncoder|PasswordEncoder|AuthenticationManager|UserManager|SignInManager|RoleManager|JwtSecurityTokenHandler|IPasswordHasher|PasswordHasher|KeycloakClient|OAuth2AuthorizedClientService)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Queue,
        r#"(?:Queue|IQueue|Producer|IProducer|Publisher|IPublisher|IPublishEndpoint|ISendEndpoint|IBus|IMessageBus|MessageBus|EventBus|IEventBus|Channel|KafkaTemplate|RabbitTemplate|JmsTemplate|StreamBridge|SqsClient|SnsClient|AmazonSQS|AmazonSNS|ApplicationEventPublisher|EventEmitter2|IBackgroundJobClient|BackgroundJobClient|Agenda|PgBoss)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Email,
        r#"(?:MailerService|JavaMailSender|MailSender|IEmailSender|SendGridClient|SESClient|AmazonSimpleEmailService|Transporter|Resend|PostmarkClient)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Payments,
        r#"(?:Stripe|StripeClient|BraintreeGateway|PayPalClient|Razorpay|Adyen|Mollie|Paddle|Chargebee)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Cache,
        r#"(?:CacheManager|IMemoryCache|IDistributedCache|RedisTemplate|StringRedisTemplate|Redis|RedisClient|Jedis|Lettuce|RedissonClient|MemcachedClient|IConnectionMultiplexer|ConnectionMultiplexer|IDatabase)(?:<[^>]*>)?$"#,
    ),
    (
        EffectCategory::Storage,
        r#"(?:S3Client|AmazonS3|BlobClient|BlobServiceClient|BlobContainerClient|MinioClient|IFileProvider|Cloudinary|FileSystem|IFileSystem)(?:<[^>]*>)?$"#,
    ),
];

/// A pattern compiled once.
fn compiled(pattern: &'static str, slot: &'static OnceLock<Regex>) -> &'static Regex {
    slot.get_or_init(|| Regex::new(pattern).expect("an effects pattern compiles"))
}

macro_rules! pattern {
    ($name:ident, $source:expr) => {
        fn $name() -> &'static Regex {
            static SLOT: OnceLock<Regex> = OnceLock::new();
            compiled($source, &SLOT)
        }
    };
}

pattern!(exception_response, EXCEPTION_RESPONSE);
pattern!(not_a_response, NOT_A_RESPONSE);
pattern!(read_ops, READ_OPS);
pattern!(write_ops, WRITE_OPS);
pattern!(builtin_receivers, BUILTIN_RECEIVERS);
pattern!(model_suffix, MODEL_SUFFIX);
pattern!(swift_not_an_effect, SWIFT_NOT_AN_EFFECT);
pattern!(argument_lists, r"\([^()]*\)");

/// The rows' patterns, compiled once, in table order.
fn rule_patterns() -> &'static [Option<Regex>] {
    static SLOT: OnceLock<Vec<Option<Regex>>> = OnceLock::new();
    SLOT.get_or_init(|| {
        EFFECT_RULES
            .iter()
            .map(|rule| match rule.test {
                Test::Pattern(source) => Some(Regex::new(source).expect("an effects row compiles")),
                Test::ModelNotViewModel | Test::OrmStaticNotBuiltin => None,
            })
            .collect()
    })
}

/// [`Test::ModelNotViewModel`]: the receiver segment is captured whole, and
/// what comes before its model word must not end in `View` / `view`.
fn model_not_view_model(text: &str) -> bool {
    static SLOT: OnceLock<Regex> = OnceLock::new();
    let pattern = SLOT.get_or_init(|| {
        Regex::new(&format!(
            r"^(?:this\.)?(_?[0-9A-Za-z_]*)(?:Model|Entity|Collection|Table|Db|DB|Database|Datastore)\.(?:[0-9A-Za-z_]+\.)?{DB_OP}$"
        ))
        .expect("the model row compiles")
    });
    pattern.captures(text).is_some_and(|captures| {
        let before = &captures[1];
        !before.ends_with("View") && !before.ends_with("view")
    })
}

/// [`Test::OrmStaticNotBuiltin`]: the receiver is captured whole, and must
/// not be one of the built-ins (a receiver equal to a built-in is exactly
/// where upstream's `(?!BUILTIN\b)` refuses).
fn orm_static_not_builtin(text: &str) -> bool {
    static SLOT: OnceLock<Regex> = OnceLock::new();
    let pattern = SLOT.get_or_init(|| {
        Regex::new(&format!(r"^([A-Z][0-9A-Za-z_]*)\.{ORM_STATIC_OPS}$"))
            .expect("the static ORM row compiles")
    });
    pattern
        .captures(text)
        .is_some_and(|captures| !builtin_receivers().is_match(&captures[1]))
}

fn rule_matches(index: usize, rule: &EffectRule, text: &str) -> bool {
    match rule.test {
        Test::Pattern(_) => rule_patterns()[index]
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text)),
        Test::ModelNotViewModel => model_not_view_model(text),
        Test::OrmStaticNotBuiltin => orm_static_not_builtin(text),
    }
}

/// `text` without the leading keyword `words` names and the whitespace
/// around it (`/^\s*(?:words)\s+/`).
fn strip_keyword<'t>(text: &'t str, words: &[&str]) -> &'t str {
    let trimmed = text.trim_start();
    for word in words {
        if let Some(rest) = trimmed.strip_prefix(word)
            && rest.starts_with(char::is_whitespace)
        {
            return rest.trim_start();
        }
    }
    text
}

/// The call text the rows see: `await` / `new` dropped, `?.` and `!.` as
/// `.`, whitespace and type arguments gone, `this.` kept.
pub fn normalise_call(text: &str) -> String {
    let text = strip_keyword(text, &["await", "new", "yield", "return", "throw"]);
    let text = strip_keyword(text, &["await", "new"]);
    let text = text.replace("?.", ".").replace("!.", ".");
    let text: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    static TYPE_ARGUMENTS: OnceLock<Regex> = OnceLock::new();
    TYPE_ARGUMENTS
        .get_or_init(|| Regex::new(r"<[^<>]*>").expect("valid regex"))
        .replace_all(&text, "")
        .into_owned()
}

/// `text` with every argument list that holds none emptied away, one pass.
fn without_argument_lists(text: &str) -> String {
    argument_lists().replace_all(text, "").into_owned()
}

/// The last `.`- or `:`-separated piece.
fn last_piece(text: &str) -> &str {
    text.rsplit(['.', ':']).next().unwrap_or(text)
}

/// What a call is, when it is one of the things the table names; `None` for
/// everything else — a plain call into a library is not an effect.
pub fn classify_effect(input: &EffectInput<'_>) -> Option<Effect> {
    // Rows see the chain without its argument lists: `res.status(404).json`
    // is `res.status.json`.
    let text = without_argument_lists(&normalise_call(input.text));
    if text.is_empty() {
        return None;
    }
    let family = family_of(input.language);
    if family == Some(Family::Swift) && swift_not_an_effect().is_match(&text) {
        return None;
    }
    for (index, rule) in EFFECT_RULES.iter().enumerate() {
        if input.project_type
            && !matches!(
                rule.category,
                EffectCategory::Database | EffectCategory::Response
            )
        {
            continue;
        }
        if !rule.only.is_empty() && !family.is_some_and(|family| rule.only.contains(&family)) {
            // A call with no language still gets the JS rows.
            if input.language.is_some() || !rule.only.contains(&Family::Js) {
                continue;
            }
        }
        if rule.instantiates && input.kind != CallKind::Instantiates {
            continue;
        }
        if rule_matches(index, rule, &text) {
            return Some(with_shape(
                rule.category,
                &text,
                input.args,
                input.language,
                None,
            ));
        }
    }
    // The receiver's declared type — a library's, never a class of the
    // project (the caller checks).
    if let Some(receiver_type) = input.receiver_type.filter(|text| !text.is_empty()) {
        let type_name = strip_modifier(receiver_type).trim();
        for (category, pattern) in receiver_type_rules() {
            if pattern.is_match(type_name) {
                return Some(with_shape(
                    *category,
                    &text,
                    input.args,
                    input.language,
                    Some(type_name),
                ));
            }
        }
    }
    // A thrown web exception is a response, in a project that has endpoints.
    if input.project != ProjectKind::App
        && (input.kind == CallKind::Instantiates
            || matches!(family, Some(Family::Py | Family::Swift)))
    {
        // `\InvalidArgumentException`, `Foo\BarException`: the class, not
        // its namespace.
        let last = last_piece(&text);
        let last = last.rsplit('\\').next().unwrap_or(last);
        if exception_response().is_match(&text) && !not_a_response().is_match(last) {
            return Some(Effect::of(EffectCategory::Response));
        }
    }
    None
}

/// One leading `readonly` / `private` / … word gone
/// (`/^(?:readonly|private|public|protected|final|static)\s+/g`, whose `^`
/// matches once).
fn strip_modifier(text: &str) -> &str {
    for word in [
        "readonly",
        "private",
        "public",
        "protected",
        "final",
        "static",
    ] {
        if let Some(rest) = text.strip_prefix(word)
            && rest.starts_with(char::is_whitespace)
        {
            return rest.trim_start();
        }
    }
    text
}

/// [`RECEIVER_TYPE_RULES`], compiled once.
fn receiver_type_rules() -> &'static [(EffectCategory, Regex)] {
    static SLOT: OnceLock<Vec<(EffectCategory, Regex)>> = OnceLock::new();
    SLOT.get_or_init(|| {
        RECEIVER_TYPE_RULES
            .iter()
            .map(|(category, source)| {
                (
                    *category,
                    Regex::new(source).expect("a receiver-type row compiles"),
                )
            })
            .collect()
    })
}

/// The category plus what the text says about the model and the access.
fn with_shape(
    category: EffectCategory,
    text: &str,
    args: Option<&str>,
    language: Option<Language>,
    receiver_type: Option<&str>,
) -> Effect {
    if category != EffectCategory::Database {
        return Effect::of(category);
    }
    let bare = without_argument_lists(text);
    let segments: Vec<&str> = bare
        .split(['.', ':'])
        .filter(|segment| !segment.is_empty())
        .collect();
    let last = segments.last().copied().unwrap_or("");
    let op = last.strip_suffix(['!', '?']).unwrap_or(last);
    let access = if op == "exec" && language == Some(Language::Python) {
        Some(Access::Read)
    } else if write_ops().is_match(op) {
        Some(Access::Write)
    } else if read_ops().is_match(op) {
        Some(Access::Read)
    } else {
        None
    };
    Effect {
        category,
        model: model_of(&segments, args, receiver_type),
        access,
    }
}

/// `/^[A-Za-z_$][\w$]*$/`.
fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

/// `text` without one model suffix at its end.
fn strip_model_suffix(text: &str) -> String {
    model_suffix().replace(text, "").into_owned()
}

/// The table / model / collection a database call names.
fn model_of(segments: &[&str], args: Option<&str>, receiver_type: Option<&str>) -> Option<String> {
    let no_this = match segments.first() {
        Some(&"this" | &"self") => &segments[1..],
        _ => segments,
    };
    let first = no_this.first().copied().unwrap_or("");
    let second_last = if no_this.len() >= 3 {
        no_this[no_this.len() - 2]
    } else {
        ""
    };
    // `prisma.user.create`, `db.users.insert`, `_context.TodoItems.Add`.
    if no_this.len() >= 3
        && is_name(second_last)
        && !matches!(
            second_last,
            "objects"
                | "query"
                | "session"
                | "db"
                | "$"
                | "from"
                | "table"
                | "collection"
                | "opsForValue"
                | "Items"
        )
        && !model_suffix().is_match(second_last)
        && second_last != "$transaction"
    {
        let lower = first.to_ascii_lowercase();
        let known = matches!(
            lower.as_str(),
            "prisma"
                | "db"
                | "database"
                | "orm"
                | "em"
                | "drizzle"
                | "sql"
                | "_context"
                | "context"
                | "dbcontext"
                | "_db"
                | "ctx"
                | "this"
        );
        let client = ["Context", "Client", "Prisma", "Db", "DB"]
            .iter()
            .any(|suffix| first.ends_with(suffix));
        if known || client {
            return Some(second_last.to_string());
        }
    }
    // `usersRepository.save`, `_orderRepository.AddAsync`, `userModel.find`.
    let receiver = if no_this.len() >= 2 { no_this[0] } else { "" };
    if !receiver.is_empty()
        && model_suffix().is_match(receiver)
        && !matches!(receiver, "this" | "self")
    {
        let stem = strip_model_suffix(receiver.trim_start_matches('_'));
        if !stem.is_empty() {
            return Some(stem);
        }
    }
    // `User.objects.filter`, `User.find`, `Todo.query(on:)`, `User::find`.
    if no_this.len() >= 2
        && first.starts_with(|ch: char| ch.is_ascii_uppercase())
        && first.chars().all(|ch| ch.is_ascii_alphanumeric())
        && !matches!(
            first,
            "DB" | "ActiveRecord" | "Files" | "File" | "Path" | "Base"
        )
    {
        return Some(first.to_string());
    }
    // Spring: `owners.save` where `owners` is an `OwnerRepository`.
    if let Some(receiver_type) = receiver_type {
        static GENERICS: OnceLock<Regex> = OnceLock::new();
        let plain = GENERICS
            .get_or_init(|| Regex::new(r"<[^>]*>").expect("valid regex"))
            .replace_all(receiver_type, "")
            .into_owned();
        // `/^I(?=[A-Z])/`: an interface's prefix.
        let plain = match plain.strip_prefix('I') {
            Some(rest) if rest.starts_with(|ch: char| ch.is_ascii_uppercase()) => rest.to_string(),
            _ => plain,
        };
        let stem = strip_model_suffix(&plain);
        if !stem.is_empty()
            && stem != receiver_type
            && stem.starts_with(|ch: char| ch.is_ascii_uppercase())
            && !matches!(
                stem.as_str(),
                "Entity" | "Db" | "Data" | "Jdbc" | "Mongo" | "Session" | "Async"
            )
        {
            return Some(stem);
        }
    }
    // `knex('users')`, `db.from('users')`, `collection('todos')`.
    if let Some(args) = args {
        let last = no_this.last().copied().unwrap_or("").to_ascii_lowercase();
        if matches!(
            last.as_str(),
            "knex"
                | "db"
                | "collection"
                | "table"
                | "from"
                | "into"
                | "selectfrom"
                | "insertinto"
                | "updatetable"
                | "deletefrom"
                | "sql.table"
                | "query"
                | "getrepository"
                | "getcollection"
                | "model"
        ) {
            static QUOTED: OnceLock<Regex> = OnceLock::new();
            if let Some(captures) = QUOTED
                .get_or_init(|| Regex::new(r#"^['"`]([0-9A-Za-z_.-]+)['"`]"#).expect("valid regex"))
                .captures(args)
            {
                return Some(captures[1].to_string());
            }
        }
    }
    None
}

// =============================================================================
// Response status
// =============================================================================

/// Upstream `STATUS_BY_NAME`, in its order (a name ending a class name is
/// looked up in this order).
const STATUS_BY_NAME: &[(&str, u16)] = &[
    ("ok", 200),
    ("success", 200),
    ("created", 201),
    ("accepted", 202),
    ("nocontent", 204),
    ("resetcontent", 205),
    ("partialcontent", 206),
    ("movedpermanently", 301),
    ("found", 302),
    ("redirect", 302),
    ("seeother", 303),
    ("notmodified", 304),
    ("temporaryredirect", 307),
    ("permanentredirect", 308),
    ("badrequest", 400),
    ("unauthorized", 401),
    ("unauthenticated", 401),
    ("paymentrequired", 402),
    ("forbidden", 403),
    ("notfound", 404),
    ("methodnotallowed", 405),
    ("notacceptable", 406),
    ("proxyauthenticationrequired", 407),
    ("requesttimeout", 408),
    ("conflict", 409),
    ("gone", 410),
    ("lengthrequired", 411),
    ("preconditionfailed", 412),
    ("payloadtoolarge", 413),
    ("requestentitytoolarge", 413),
    ("uritoolong", 414),
    ("unsupportedmediatype", 415),
    ("rangenotsatisfiable", 416),
    ("expectationfailed", 417),
    ("imateapot", 418),
    ("misdirectedrequest", 421),
    ("unprocessableentity", 422),
    ("unprocessable", 422),
    ("locked", 423),
    ("faileddependency", 424),
    ("tooearly", 425),
    ("upgraderequired", 426),
    ("preconditionrequired", 428),
    ("toomanyrequests", 429),
    ("throttled", 429),
    ("requestheaderfieldstoolarge", 431),
    ("unavailableforlegalreasons", 451),
    ("internalservererror", 500),
    ("internalerror", 500),
    ("servererror", 500),
    ("internalserver", 500),
    ("notimplemented", 501),
    ("badgateway", 502),
    ("serviceunavailable", 503),
    ("gatewaytimeout", 504),
    ("httpversionnotsupported", 505),
    ("insufficientstorage", 507),
    ("loopdetected", 508),
    ("networkauthenticationrequired", 511),
    ("validationproblem", 400),
    ("problem", 500),
    ("forbid", 403),
    ("challenge", 401),
    ("entitynotfound", 404),
];

/// `/^\d{3}$/`, as a number.
fn three_digits(text: &str) -> Option<u16> {
    (text.len() == 3 && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn status_of_name(raw: &str) -> Option<u16> {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    static SUFFIX: OnceLock<Regex> = OnceLock::new();
    static CODE: OnceLock<Regex> = OnceLock::new();
    static EMBEDDED: OnceLock<Regex> = OnceLock::new();
    let name = PREFIX
        .get_or_init(|| {
            Regex::new(r"^(?:HttpStatus|HTTPStatus|StatusCodes|StatusCode|http|status|HttpStatusCode|Status|HTTP_|HTTP)[._]?")
                .expect("valid regex")
        })
        .replace(raw, "")
        .into_owned();
    let name = SUFFIX
        .get_or_init(|| {
            Regex::new(r"(?:Exception|Error|Response|Result|Async|Http|Status|Code)$")
                .expect("valid regex")
        })
        .replace(&name, "")
        .into_owned();
    // `/^Http(?=[A-Z])/`.
    let name = match name.strip_prefix("Http") {
        Some(rest) if rest.starts_with(|ch: char| ch.is_ascii_uppercase()) => rest.to_string(),
        _ => name,
    };
    let key: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(code) = three_digits(&key) {
        return Some(code);
    }
    let found = CODE
        .get_or_init(|| Regex::new(r"^(?:http)?([0-9]{3})$").expect("valid regex"))
        .captures(&key)
        .or_else(|| {
            EMBEDDED
                .get_or_init(|| {
                    Regex::new(r"(?:^|[a-z_])([1-5][0-9]{2})(?:[a-z_]|$)").expect("valid regex")
                })
                .captures(&key)
        });
    if let Some(found) = found {
        return found[1].parse().ok();
    }
    if let Some((_, code)) = STATUS_BY_NAME.iter().find(|(name, _)| *name == key) {
        return Some(*code);
    }
    // `UserNotFound`, `InvalidTokenUnauthorized`: the status name ends the
    // class name.
    STATUS_BY_NAME
        .iter()
        .find(|(known, _)| known.len() >= 6 && key.ends_with(known))
        .map(|(_, code)| *code)
}

/// The abbreviated argument list split on its top-level commas.
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in args.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch == ',' && depth == 0 {
            out.push(current.trim().to_string());
            current.clear();
            continue;
        }
        current.push(ch);
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

fn literal_status(token: &str) -> Option<u16> {
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static DOT: OnceLock<Regex> = OnceLock::new();
    static PREFIXED: OnceLock<Regex> = OnceLock::new();
    static CONSTANT: OnceLock<Regex> = OnceLock::new();
    let token = token.trim();
    if let Some(code) = three_digits(token) {
        return Some(code);
    }
    let named = NAMED
        .get_or_init(|| {
            Regex::new(r"^(?:HttpStatus|HTTPStatus|StatusCodes|HttpStatusCode|http|status|Status|HttpStatusCodes|StatusCode|HTTPResponseStatus)\.([0-9A-Za-z_]+)$")
                .expect("valid regex")
        })
        .captures(token)
        .or_else(|| {
            DOT.get_or_init(|| Regex::new(r"^\.([0-9A-Za-z_]+)$").expect("valid regex"))
                .captures(token)
        })
        .or_else(|| {
            PREFIXED
                .get_or_init(|| {
                    Regex::new(r"^(?:HTTP_|StatusCodes\.)([0-9A-Za-z_]+)$").expect("valid regex")
                })
                .captures(token)
        });
    if let Some(named) = named {
        return status_of_name(&named[1]);
    }
    if CONSTANT
        .get_or_init(|| Regex::new(r"^[A-Z][A-Z_]+$").expect("valid regex"))
        .is_match(token)
    {
        return status_of_name(token);
    }
    None
}

/// The status a response site sends, when it is literal: the number in
/// `res.status(404)`, the name in `ResponseEntity.notFound()`, `throw new
/// NotFoundException()`, `c.JSON(http.StatusCreated, …)`,
/// `HTTPException(status_code=404)`, `Abort(.notFound)`. `None` when the
/// code is a variable or the site sets none.
pub fn response_status(text: &str, args: Option<&str>) -> Option<u16> {
    static IN_CHAIN: OnceLock<Regex> = OnceLock::new();
    static STATUS_CALLEE: OnceLock<Regex> = OnceLock::new();
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static MEMBER: OnceLock<Regex> = OnceLock::new();
    static EXCEPTION_LIKE: OnceLock<Regex> = OnceLock::new();
    static REPLY_WORD: OnceLock<Regex> = OnceLock::new();
    static REDIRECT: OnceLock<Regex> = OnceLock::new();
    static LARAVEL_REDIRECT: OnceLock<Regex> = OnceLock::new();
    let call = normalise_call(text);
    // `res.status(404).json`, `reply.code(201).send`.
    if let Some(found) = IN_CHAIN
        .get_or_init(|| {
            Regex::new(r"(?:^|\.)(?:status|sendStatus|code|Status|StatusCode|SendStatus|withStatus|with_status)\(([^()]+)\)")
                .expect("valid regex")
        })
        .captures(&call)
        && let Some(code) = literal_status(&found[1])
    {
        return Some(code);
    }
    let bare = without_argument_lists(&call);
    let last = last_piece(&bare);
    let last = last.strip_prefix("new").unwrap_or(last);
    let args = args.unwrap_or("");
    // The status is the argument: `res.status(404)`, `abort(404)`,
    // `c.String(200, …)`, `http.Error(w, m, 500)`.
    if STATUS_CALLEE
        .get_or_init(|| {
            Regex::new(r"^(?:status|sendStatus|code|abort|StatusCode|Status|SendStatus|WriteHeader|String|Data|JSON|IndentedJSON|XML|YAML|HTML|AbortWithStatus|AbortWithStatusJSON|Error|Redirect|head|Problem|error)$")
                .expect("valid regex")
        })
        .is_match(last)
    {
        let parts = split_args(args);
        let token = if matches!(last, "Error" | "Redirect") {
            parts.last()
        } else {
            parts.first()
        };
        if let Some(code) = token.and_then(|token| literal_status(token)) {
            return Some(code);
        }
        if last == "Redirect" {
            return Some(302);
        }
        if matches!(last, "Problem" | "error") {
            return Some(500);
        }
    }
    // `status_code=404`, `statusCode: 404` anywhere in the arguments.
    if let Some(found) = KEYWORD
        .get_or_init(|| {
            Regex::new(r"(?:status_code|statusCode|status|code)\s*[=:]\s*([0-9A-Za-z_.]+)")
                .expect("valid regex")
        })
        .captures(args)
        && let Some(code) = literal_status(&found[1])
    {
        return Some(code);
    }
    let named = NAMED
        .get_or_init(|| {
            Regex::new(r"(?-u:\b)(?:HttpStatus|HTTPStatus|StatusCodes|HttpStatusCode|http)\.([0-9A-Za-z_]+)")
                .expect("valid regex")
        })
        .captures(args)
        .or_else(|| {
            MEMBER
                .get_or_init(|| Regex::new(r"(?:^|[\s,(])\.([0-9A-Za-z_]+)").expect("valid regex"))
                .captures(args)
        });
    if let Some(named) = named
        && let Some(code) = status_of_name(&named[1])
    {
        return Some(code);
    }
    // An exception or a reply takes its status wherever the argument list
    // carries one literal.
    if EXCEPTION_LIKE
        .get_or_init(|| {
            Regex::new(r"Exception$|Error$|^Abort$|^abort$|^HTTPError$").expect("valid regex")
        })
        .is_match(last)
    {
        for part in split_args(args) {
            if let Some(code) = literal_status(&part) {
                return Some(code);
            }
        }
    }
    // The name says it: `NotFoundException`, `ResponseEntity.notFound().build`,
    // `TypedResults.NoContent`, `Ok`, `Http404`.
    let reply_word = REPLY_WORD.get_or_init(|| {
        Regex::new(r"^(?:json|send|end|render|view|View|Json|Content|File|jsonify|JSONResponse|Response|HttpResponse|JsonResponse|render_template|body|text|html|build|status|type|header|headers|set|res|response|reply|rep|ctx|c|context|this|self|http|w|rw|ResponseEntity|Results|TypedResults|HttpStatus|NextResponse)$")
            .expect("valid regex")
    });
    for segment in bare
        .split(['.', ':'])
        .filter(|segment| !segment.is_empty())
        .rev()
    {
        if reply_word.is_match(segment) {
            continue;
        }
        if let Some(code) = status_of_name(segment) {
            return Some(code);
        }
        break;
    }
    if REDIRECT
        .get_or_init(|| {
            Regex::new(r"^(?:redirect|redirect_to|RedirectResponse|HttpResponseRedirect|RedirectToAction|RedirectToPage|RedirectToRoute|LocalRedirect|permanentRedirect|Redirect|RedirectView)$")
                .expect("valid regex")
        })
        .is_match(last)
    {
        return Some(if last.to_ascii_lowercase().contains("permanent") {
            308
        } else {
            302
        });
    }
    // `new ModelAndView("redirect:/owners")` redirects.
    if last == "ModelAndView" && args.trim_start().starts_with("\"redirect:") {
        return Some(302);
    }
    // Laravel: `redirect()->route('home')`, `back()`, `to_route('home')`.
    if LARAVEL_REDIRECT
        .get_or_init(|| {
            Regex::new(r"^(?:redirect->[0-9A-Za-z_]+|back|to_route)(?:->[0-9A-Za-z_]+)*$")
                .expect("valid regex")
        })
        .is_match(&bare)
    {
        return Some(302);
    }
    if matches!(
        last,
        "Created" | "CreatedAtAction" | "CreatedAtRoute" | "created"
    ) {
        return Some(201);
    }
    None
}

/// The status a reply sends when it sets none — 200 — for the calls that
/// send a body and default to it. `None` when the chain sets a status of its
/// own (a variable code is unknown, not 200), when the call ends a response
/// without a body, or when the call is not one of these.
pub fn implicit_response_status(text: &str) -> Option<u16> {
    static SETS_STATUS: OnceLock<Regex> = OnceLock::new();
    static BODY_REPLY: OnceLock<Regex> = OnceLock::new();
    static OTHERS: OnceLock<Regex> = OnceLock::new();
    let call = normalise_call(text);
    if SETS_STATUS
        .get_or_init(|| {
            Regex::new(r"(?:^|\.)(?:status|sendStatus|code|Status|StatusCode|SendStatus|withStatus|with_status|writeHead)\(")
                .expect("valid regex")
        })
        .is_match(&call)
    {
        return None;
    }
    let bare = without_argument_lists(&call);
    if BODY_REPLY
        .get_or_init(|| {
            Regex::new(r"^(?:res|response|reply|rep|ctx|c|context)(?:\.(?:type|set|header|headers|append|cookie|clearCookie|vary|location|links|format))*\.(?:json|jsonp|send|render|sendFile|download|text|html|body|stream|file|view)$")
                .expect("valid regex")
        })
        .is_match(&bare)
    {
        return Some(200);
    }
    if OTHERS
        .get_or_init(|| {
            Regex::new(r"^(?:NextResponse|Response)\.json$|^(?:JSONResponse|HTMLResponse|PlainTextResponse|ORJSONResponse|UJSONResponse|jsonify|render_template|render|make_response|HttpResponse|JsonResponse|send_file|send_from_directory)$|^(?:render|render_to_string|respond_with)$|^ModelAndView$")
                .expect("valid regex")
        })
        .is_match(&bare)
    {
        return Some(200);
    }
    let squeezed: String = call.chars().filter(|ch| !ch.is_whitespace()).collect();
    if matches!(
        squeezed.as_str(),
        "response()->json" | "response()->view" | "response->json" | "view"
    ) {
        return Some(200);
    }
    None
}

/// The word the legend and a box's sub line use for a category.
pub fn category_word(category: EffectCategory) -> &'static str {
    category.as_str()
}

#[cfg(test)]
mod tests {
    //! Ported from upstream `__tests__/ui-effects.test.ts` (all of it) and
    //! `__tests__/swiftui-viewer-noise.test.ts:25-58`, at `e8ddc4e2`; the
    //! expected values are upstream's.
    use super::*;

    fn language(name: &str) -> Language {
        serde_json::from_value(serde_json::Value::String(name.to_string()))
            .unwrap_or_else(|_| panic!("a language named {name}"))
    }

    fn classify(
        text: &str,
        kind: CallKind,
        lang: Option<&str>,
        project: ProjectKind,
        receiver_type: Option<&str>,
        args: Option<&str>,
        project_type: bool,
    ) -> Option<Effect> {
        classify_effect(&EffectInput {
            text,
            kind,
            language: lang.map(language),
            project,
            receiver_type,
            args,
            project_type,
        })
    }

    /// Upstream's `c(text, language)`: a call, in an API project.
    fn c(text: &str, lang: &str) -> Option<Effect> {
        classify(
            text,
            CallKind::Calls,
            Some(lang),
            ProjectKind::Api,
            None,
            None,
            false,
        )
    }

    /// A call with no language at all.
    fn c0(text: &str) -> Option<Effect> {
        classify(
            text,
            CallKind::Calls,
            None,
            ProjectKind::Api,
            None,
            None,
            false,
        )
    }

    /// Upstream's `n(text, language)`: an instantiation, in an API project.
    fn n(text: &str, lang: &str) -> Option<Effect> {
        classify(
            text,
            CallKind::Instantiates,
            Some(lang),
            ProjectKind::Api,
            None,
            None,
            false,
        )
    }

    fn category(effect: Option<Effect>) -> Option<&'static str> {
        effect.map(|effect| effect.category.as_str())
    }

    fn db(model: Option<&str>, access: Option<Access>) -> Option<Effect> {
        Some(Effect {
            category: EffectCategory::Database,
            model: model.map(str::to_string),
            access,
        })
    }

    const READ: Option<Access> = Some(Access::Read);
    const WRITE: Option<Access> = Some(Access::Write);

    #[test]
    fn keeps_the_mobile_apps_categories_with_or_without_a_language() {
        assert_eq!(category(c0("client.post")), Some("network"));
        assert_eq!(category(c("fetch", "tsx")), Some("network"));
        assert_eq!(category(c("AsyncStorage.setItem", "tsx")), Some("storage"));
        assert_eq!(category(c("Linking.openURL", "tsx")), Some("device"));
        assert_eq!(category(c("DdRum.addAction", "tsx")), Some("telemetry"));
        assert_eq!(c("Math.max", "tsx"), None);
        assert_eq!(c("i18n.t", "tsx"), None);
        assert_eq!(c("Object.create", "typescript"), None);
    }

    #[test]
    fn typescript_servers_the_database_by_the_chain_the_model_and_the_access() {
        assert_eq!(
            c("prisma.article.findFirst", "typescript"),
            db(Some("article"), READ)
        );
        assert_eq!(
            c("this.prisma.user.create", "typescript"),
            db(Some("user"), WRITE)
        );
        assert_eq!(
            c("this.usersRepository.save", "typescript"),
            db(Some("users"), WRITE)
        );
        assert_eq!(c("this.catModel.find", "typescript"), db(Some("cat"), READ));
        assert_eq!(c("db.insert", "typescript"), db(None, WRITE));
        assert_eq!(
            classify(
                "knex",
                CallKind::Calls,
                Some("typescript"),
                ProjectKind::Api,
                None,
                Some("'users'"),
                false
            ),
            None
        );
        assert_eq!(c("User.findOne", "typescript"), db(Some("User"), READ));
        assert_eq!(c("Promise.all", "typescript"), None);
    }

    #[test]
    fn typescript_servers_responses_queues_email_payments_cache_auth() {
        let ts = |text: &str| category(c(text, "typescript"));
        assert_eq!(ts("res.status(404).json"), Some("response"));
        assert_eq!(ts("res.json"), Some("response"));
        assert_eq!(ts("reply.code(201).send"), Some("response"));
        assert_eq!(ts("c.json"), Some("response"));
        assert_eq!(
            category(n("NotFoundException", "typescript")),
            Some("response")
        );
        assert_eq!(
            category(n("UnprocessableEntityException", "typescript")),
            Some("response")
        );
        assert_eq!(category(n("HttpException", "typescript")), Some("response"));
        assert_eq!(n("Error", "typescript"), None);
        assert_eq!(n("TypeError", "typescript"), None);
        // In an app, an exception is an error, not a reply.
        assert_eq!(
            classify(
                "ValidationException",
                CallKind::Instantiates,
                Some("typescript"),
                ProjectKind::App,
                None,
                None,
                false
            ),
            None
        );
        assert_eq!(ts("this.emailQueue.add"), Some("queue"));
        assert_eq!(ts("queue.add"), Some("queue"));
        assert_eq!(ts("this.mailerService.sendMail"), Some("email"));
        assert_eq!(ts("resend.emails.send"), Some("email"));
        assert_eq!(ts("stripe.checkout.sessions.create"), Some("payments"));
        assert_eq!(ts("this.cacheManager.get"), Some("cache"));
        assert_eq!(ts("redis.setex"), Some("cache"));
        assert_eq!(ts("this.jwtService.signAsync"), Some("auth"));
        assert_eq!(ts("bcrypt.compare"), Some("auth"));
        assert_eq!(ts("jwt.verify"), Some("auth"));
        assert_eq!(ts("crypto.createHmac"), Some("auth"));
        assert_eq!(ts("crypto.createHash"), None);
        assert_eq!(ts("crypto.randomBytes"), None);
        assert_eq!(ts("s3.putObject"), Some("storage"));
        assert_eq!(ts("fs.writeFile"), Some("storage"));
        assert_eq!(ts("spawn"), Some("process"));
        assert_eq!(ts("process.exit"), Some("process"));
    }

    #[test]
    fn python_sqlalchemy_django_fastapi_flask_django_responses_celery_files_processes() {
        let py = |text: &str| c(text, "python");
        assert_eq!(py("session.exec"), db(None, READ));
        assert_eq!(py("session.add"), db(None, WRITE));
        assert_eq!(py("session.commit"), db(None, WRITE));
        assert_eq!(py("User.objects.filter"), db(Some("User"), READ));
        assert_eq!(category(py("db.session.add")), Some("database"));
        for text in [
            "HTTPException",
            "JSONResponse",
            "jsonify",
            "abort",
            "render",
        ] {
            assert_eq!(category(py(text)), Some("response"), "{text}");
        }
        assert_eq!(category(py("send_email.delay")), Some("queue"));
        assert_eq!(category(py("send_mail")), Some("email"));
        assert_eq!(category(py("requests.post")), Some("network"));
        assert_eq!(category(py("httpx.AsyncClient")), Some("network"));
        assert_eq!(category(py("open")), Some("storage"));
        assert_eq!(category(py("s3.upload_file")), Some("storage"));
        assert_eq!(category(py("subprocess.run")), Some("process"));
        assert_eq!(category(py("jwt.encode")), Some("auth"));
        assert_eq!(category(py("pwd_context.verify")), Some("auth"));
        assert_eq!(py("print"), None);
        assert_eq!(py("len"), None);
        assert_eq!(py("item.model_dump"), None);
    }

    #[test]
    fn java_kotlin_repositories_by_name_and_by_declared_type_spring_responses_templates() {
        let typed = |text: &str, lang: &str| {
            classify(
                text,
                CallKind::Calls,
                Some(lang),
                ProjectKind::Api,
                Some("OwnerRepository"),
                None,
                false,
            )
        };
        assert_eq!(typed("owners.save", "java"), db(Some("Owner"), WRITE));
        assert_eq!(typed("owners.findById", "kotlin"), db(Some("Owner"), READ));
        assert_eq!(
            c("this.ownerRepository.findAll", "java"),
            db(Some("owner"), READ)
        );
        let java = |text: &str| category(c(text, "java"));
        let kotlin = |text: &str| category(c(text, "kotlin"));
        assert_eq!(java("jdbcTemplate.update"), Some("database"));
        assert_eq!(java("entityManager.persist"), Some("database"));
        assert_eq!(java("ResponseEntity.ok"), Some("response"));
        assert_eq!(
            java("ResponseEntity.status(HttpStatus.NOT_FOUND).body"),
            Some("response")
        );
        assert_eq!(
            category(n("ResponseStatusException", "java")),
            Some("response")
        );
        assert_eq!(n("IllegalArgumentException", "java"), None);
        assert_eq!(
            category(n("ResourceNotFoundException", "java")),
            Some("response")
        );
        assert_eq!(java("rabbitTemplate.convertAndSend"), Some("queue"));
        assert_eq!(java("kafkaTemplate.send"), Some("queue"));
        assert_eq!(
            java("applicationEventPublisher.publishEvent"),
            Some("queue")
        );
        assert_eq!(java("mailSender.send"), Some("email"));
        assert_eq!(java("restTemplate.getForObject"), Some("network"));
        assert_eq!(java("webClient.get"), Some("network"));
        assert_eq!(java("passwordEncoder.encode"), Some("auth"));
        assert_eq!(java("redisTemplate.opsForValue"), Some("cache"));
        assert_eq!(java("Files.write"), Some("storage"));
        // Android: DataStore, SharedPreferences, Room DAOs, WorkManager.
        assert_eq!(kotlin("userPreferences.updateData"), Some("storage"));
        assert_eq!(kotlin("sharedPreferences.edit"), Some("storage"));
        assert_eq!(
            c("topicDao.upsertTopics", "kotlin"),
            db(Some("topic"), WRITE)
        );
        assert_eq!(kotlin("workManager.enqueueUniqueWork"), Some("queue"));
        assert_eq!(kotlin("viewModelScope.launch"), None);
        assert_eq!(java("model.addAttribute"), None);
        assert_eq!(java("result.hasErrors"), None);
        assert_eq!(java("Objects.equals"), None);
    }

    #[test]
    fn csharp_ef_core_repositories_controller_responses_masstransit_identity() {
        let cs = |text: &str| c(text, "csharp");
        assert_eq!(cs("_context.TodoItems.Add"), db(Some("TodoItems"), WRITE));
        assert_eq!(cs("_context.SaveChangesAsync"), db(None, WRITE));
        assert_eq!(cs("_orderRepository.AddAsync"), db(Some("order"), WRITE));
        assert_eq!(
            cs("_basketRepository.FirstOrDefaultAsync"),
            db(Some("basket"), READ)
        );
        for text in [
            "NotFound",
            "Ok",
            "TypedResults.NoContent",
            "Results.Created",
        ] {
            assert_eq!(category(cs(text)), Some("response"), "{text}");
        }
        assert_eq!(category(n("NotFoundException", "csharp")), Some("response"));
        assert_eq!(n("ArgumentNullException", "csharp"), None);
        assert_eq!(category(cs("_bus.Publish")), Some("queue"));
        assert_eq!(category(cs("_publishEndpoint.Publish")), Some("queue"));
        assert_eq!(category(cs("BackgroundJob.Enqueue")), Some("queue"));
        assert_eq!(category(cs("_emailSender.SendEmailAsync")), Some("email"));
        assert_eq!(category(cs("_httpClient.GetAsync")), Some("network"));
        assert_eq!(category(cs("_userManager.CreateAsync")), Some("auth"));
        assert_eq!(
            category(cs("_signInManager.PasswordSignInAsync")),
            Some("auth")
        );
        assert_eq!(category(cs("_cache.GetOrCreateAsync")), Some("cache"));
        assert_eq!(category(cs("File.ReadAllText")), Some("storage"));
        assert_eq!(cs("Guard.Against.Null"), None);
        assert_eq!(cs("nameof"), None);
        assert_eq!(cs("sender.Send"), None);
    }

    #[test]
    fn go_database_sql_gorm_gin_responses_net_http_os() {
        let go = |text: &str| c(text, "go");
        assert_eq!(go("db.QueryRow"), db(None, READ));
        assert_eq!(go("db.Exec"), db(None, WRITE));
        assert_eq!(category(go("db.Create")), Some("database"));
        for text in ["c.JSON", "c.AbortWithStatus", "http.Error", "w.WriteHeader"] {
            assert_eq!(category(go(text)), Some("response"), "{text}");
        }
        assert_eq!(category(go("http.Get")), Some("network"));
        assert_eq!(category(go("client.Do")), Some("network"));
        assert_eq!(category(go("os.ReadFile")), Some("storage"));
        assert_eq!(category(go("exec.Command")), Some("process"));
        assert_eq!(category(go("producer.Produce")), Some("queue"));
        assert_eq!(category(go("jwt.NewWithClaims")), Some("auth"));
        assert_eq!(go("fmt.Sprintf"), None);
        assert_eq!(go("errors.New"), None);
    }

    #[test]
    fn c_files_sockets_processes() {
        let cc = |text: &str| category(c(text, "c"));
        for text in ["fopen", "fprintf", "write"] {
            assert_eq!(cc(text), Some("storage"), "{text}");
        }
        for text in ["socket", "connect", "curl_easy_perform"] {
            assert_eq!(cc(text), Some("network"), "{text}");
        }
        for text in ["fork", "exit", "pthread_create"] {
            assert_eq!(cc(text), Some("process"), "{text}");
        }
        for text in ["strlen", "malloc", "memcpy", "serverLog"] {
            assert_eq!(cc(text), None, "{text}");
        }
    }

    #[test]
    fn swift_vapor_ruby_rails_php_laravel() {
        assert_eq!(category(c("Abort", "swift")), Some("response"));
        assert_eq!(c("Todo.query", "swift"), db(Some("Todo"), READ));
        assert_eq!(category(c("todo.save", "swift")), Some("database"));
        assert_eq!(
            category(c("URLSession.shared.dataTask", "swift")),
            Some("network")
        );
        assert_eq!(category(c("render", "ruby")), Some("response"));
        assert_eq!(category(c("redirect_to", "ruby")), Some("response"));
        assert_eq!(c("User.find_by", "ruby"), db(Some("User"), READ));
        assert_eq!(category(c("@user.save", "ruby")), Some("database"));
        assert_eq!(category(c("UserMailer.welcome", "ruby")), Some("email"));
        assert_eq!(category(c("HardJob.perform_later", "ruby")), Some("queue"));
        assert_eq!(c("User::find", "php"), db(Some("User"), READ));
        assert_eq!(category(c("DB::table", "php")), Some("database"));
        assert_eq!(category(c("abort", "php")), Some("response"));
        assert_eq!(category(c("Mail::to", "php")), Some("email"));
    }

    #[test]
    fn a_language_without_rows_for_a_family_stays_quiet() {
        assert_eq!(c("foo.bar", "ruby"), None);
        assert_eq!(c("save", "python"), None);
        assert_eq!(c("render", "java"), None);
    }

    #[test]
    fn response_status_reads_the_literal_code_out_of_the_chain_the_arguments_or_the_name() {
        let s = |text: &str, args: &str| response_status(text, Some(args));
        assert_eq!(s("res.status(404).json", "{ error }"), Some(404));
        assert_eq!(s("res.status", "404"), Some(404));
        assert_eq!(s("res.sendStatus", "204"), Some(204));
        assert_eq!(s("res.json", "{ user }"), None);
        assert_eq!(s("reply.code(201).send", "user"), Some(201));
        assert_eq!(s("res.redirect", "'/login'"), Some(302));
        assert_eq!(s("NotFoundException", "'no such user'"), Some(404));
        assert_eq!(s("UnprocessableEntityException", "{ errors }"), Some(422));
        assert_eq!(s("HttpException", "'x', HttpStatus.FORBIDDEN"), Some(403));
        assert_eq!(s("HttpException", "'x', 418"), Some(418));
        assert_eq!(
            s("HTTPException", "status_code=404, detail=\"no title\""),
            Some(404)
        );
        assert_eq!(s("abort", "404"), Some(404));
        assert_eq!(s("JsonResponse", "{ \"error\" }, status=400"), Some(400));
        assert_eq!(s("Http404", ""), Some(404));
        assert_eq!(s("ResponseEntity.ok", "body"), Some(200));
        assert_eq!(s("ResponseEntity.notFound().build", ""), Some(404));
        assert_eq!(
            s("ResponseEntity.status(HttpStatus.CREATED).body", "saved"),
            Some(201)
        );
        assert_eq!(
            s("ResponseStatusException", "HttpStatus.NOT_FOUND, \"x\""),
            Some(404)
        );
        assert_eq!(s("ResponseEntity", "body, HttpStatus.CREATED"), Some(201));
        assert_eq!(s("NotFound", ""), Some(404));
        assert_eq!(s("Ok", "item"), Some(200));
        assert_eq!(s("CreatedAtAction", "nameof(Get), item"), Some(201));
        assert_eq!(s("TypedResults.NoContent", ""), Some(204));
        assert_eq!(s("StatusCode", "500"), Some(500));
        assert_eq!(s("Results.Problem", ""), Some(500));
        assert_eq!(s("c.JSON", "http.StatusCreated, u"), Some(201));
        assert_eq!(s("c.String", "200, \"ok\""), Some(200));
        assert_eq!(
            s("http.Error", "w, msg, http.StatusInternalServerError"),
            Some(500)
        );
        assert_eq!(s("w.WriteHeader", "http.StatusNotFound"), Some(404));
        assert_eq!(s("c.AbortWithStatus", "404"), Some(404));
        assert_eq!(s("Abort", ".notFound"), Some(404));
        assert_eq!(s("Abort", ".badRequest, reason: \"x\""), Some(400));
        assert_eq!(s("redirect_to", "root_path"), Some(302));
        assert_eq!(s("render", "json: user, status: :created"), None);
        assert_eq!(s("res.status", "code"), None);
        assert_eq!(s("res.json", ""), None);
    }

    #[test]
    fn a_body_sending_reply_that_sets_no_status_is_a_200() {
        for text in [
            "res.json",
            "res.send",
            "res.render",
            "reply.send",
            "c.json",
            "NextResponse.json",
            "JSONResponse",
            "jsonify",
        ] {
            assert_eq!(implicit_response_status(text), Some(200), "{text}");
        }
    }

    #[test]
    fn no_implicit_status_when_the_chain_sets_one_or_ends_without_a_body() {
        for text in [
            "res.status(404).json",
            "res.status(code).json",
            "res.sendStatus(204)",
            "res.end",
            "res.redirect",
            "NotFoundException",
            "prisma.user.create",
        ] {
            assert_eq!(implicit_response_status(text), None, "{text}");
        }
    }

    // `swiftui-viewer-noise.test.ts:25-58`.

    fn swift(text: &str, project_type: bool) -> Option<&'static str> {
        category(classify(
            text,
            CallKind::Calls,
            Some("swift"),
            ProjectKind::App,
            None,
            None,
            project_type,
        ))
    }

    #[test]
    fn swift_reads_network_off_a_session_not_any_data_call() {
        assert_eq!(
            swift("urlSession.data(for: request)", false),
            Some("network")
        );
        assert_eq!(
            swift("URLSession.shared.data(from: url)", false),
            Some("network")
        );
        assert_eq!(
            swift("session.upload(for: request, from: body)", false),
            Some("network")
        );
        assert_eq!(
            swift("urlSession.webSocketTask(with: url)", false),
            Some("network")
        );
        assert_eq!(swift("rawValue.data(using: .utf8)", false), None);
        assert_eq!(swift("string.data(using: .utf8)", false), None);
        assert_eq!(swift("mediaUploadService.upload(container)", false), None);
    }

    #[test]
    fn swift_foundations_timer_and_calendar_and_a_sirikit_intent_are_not_another_stacks() {
        assert_eq!(
            swift(
                "Timer.scheduledTimer(withTimeInterval: 0.05, repeats: false)",
                false
            ),
            None
        );
        assert_eq!(swift("Calendar(identifier: .gregorian)", false), None);
        assert_eq!(
            swift(
                "intent.setImage(avatar, forParameterNamed: \\.speakableName)",
                false
            ),
            None
        );
        // The same names elsewhere keep their rows.
        assert_eq!(
            category(classify(
                "Timer.builder",
                CallKind::Calls,
                Some("java"),
                ProjectKind::Api,
                None,
                None,
                false
            )),
            Some("telemetry")
        );
    }

    #[test]
    fn swift_keeps_a_view_model_out_of_the_database_and_a_keychain_in_storage() {
        assert_eq!(swift("viewModel.votes.firstIndex(of: option)", false), None);
        assert_eq!(swift("keychain.delete(key)", false), Some("storage"));
        assert_eq!(
            swift("appKeychain.set(token, forKey: key)", false),
            Some("storage")
        );
        assert_eq!(
            swift("keychainAccounts.first(where: { $0.id == id })", false),
            None
        );
        assert_eq!(
            category(classify(
                "catModel.find",
                CallKind::Calls,
                Some("typescript"),
                ProjectKind::Api,
                None,
                None,
                false
            )),
            Some("database")
        );
    }

    #[test]
    fn a_project_type_shadows_a_library_name_but_not_a_models_or_a_replys_row() {
        assert_eq!(
            swift("Notifications.notificationsV2(sinceId: id)", false),
            Some("device")
        );
        assert_eq!(
            swift("Notifications.notificationsV2(sinceId: id)", true),
            None
        );
        assert_eq!(swift("Todo.query", true), Some("database"));
        assert_eq!(
            category(classify(
                "Abort",
                CallKind::Calls,
                Some("swift"),
                ProjectKind::Api,
                None,
                None,
                true
            )),
            Some("response")
        );
    }
}
