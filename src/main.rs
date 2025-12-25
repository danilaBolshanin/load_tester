use std::time::{Duration, Instant};
use std::sync::Arc;
use std::collections::HashMap;
use load_test::HttpMethod;
use reqwest::{Client};
use serde_json::{Value};
use tokio::sync::Semaphore;
use futures::future::join_all;
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use url::Url;
use base64::{Engine as _, engine::general_purpose};
use rand::Rng;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::fs;

// Структура для распределения запросов по URL
struct MultiUrlTester {
    configs: Vec<RequestConfig>,
    distribution: UrlDistribution,
    current_index: AtomicUsize,
}

impl MultiUrlTester {
    fn new(configs: Vec<RequestConfig>, distribution: UrlDistribution) -> Self {
        Self {
            configs,
            distribution,
            current_index: AtomicUsize::new(0),
        }
    }

    fn get_next_config(&self, user_id: usize) -> &RequestConfig {
        match self.distribution {
            UrlDistribution::RoundRobin => {
                let index = self.current_index.fetch_add(1, Ordering::SeqCst);
                &self.configs[index % self.configs.len()]
            }
            UrlDistribution::Random => {
                let index = rand::thread_rng().gen_range(0..self.configs.len());
                &self.configs[index]
            }
            UrlDistribution::Sequential => {
                let url_index = (user_id - 1) % self.configs.len();
                &self.configs[url_index]
            }
            UrlDistribution::Weighted => {
                // Простая реализация взвешенного распределения
                let total_weight: u32 = self.configs.iter()
                    .map(|_| 1) // Временное значение, можно добавить веса в конфиг
                    .sum();
                let random = rand::thread_rng().gen_range(0..total_weight);
                
                let mut accumulated = 0;
                for (i, _) in self.configs.iter().enumerate() {
                    accumulated += 1; // Здесь должен быть вес URL
                    if random < accumulated {
                        return &self.configs[i];
                    }
                }
                &self.configs[0]
            }
        }
    }
}

#[derive(Parser)]
pub struct MultiUrlConfig {
    /// Configuration file with multiple URLs (JSON, YAML, or TOML)
    #[arg(short = 'f', long)]
    pub config_file: Option<String>,

    /// List of URLs to test (comma-separated)
    #[arg(short = 'L', long, value_delimiter = ',')]
    pub url_list: Option<Vec<String>>,

    /// HTTP method for all URLs
    #[arg(short = 'X', long, value_enum, default_value = "get")]
    pub method: HttpMethod,

    /// Request body (applied to all URLs)
    #[arg(short = 'd', long)]
    pub body: Option<String>,

    /// Headers (applied to all URLs)
    #[arg(short = 'H', long)]
    pub headers: Vec<String>,

    /// Content-Type
    #[arg(short = 'c', long)]
    pub content_type: Option<String>,

    /// Request timeout in seconds
    #[arg(short = 't', long, default_value_t = 30)]
    pub timeout: u64,

    /// Validate URLs before sending
    #[arg(long, default_value_t = true)]
    pub validate_url: bool,

    /// How to distribute requests between URLs
    #[arg(long, value_enum, default_value = "round-robin")]
    pub distribution: UrlDistribution,
}

#[derive(ValueEnum, Clone, Debug)]
pub enum UrlDistribution {
    RoundRobin,    // По очереди
    Random,        // Случайно
    Weighted,      // По весам
    Sequential,    // Все запросы к первому, затем ко второму и т.д.
}

// Типы body
#[derive(Debug, Clone)]
enum BodyType {
    Json(Value),
    Text(String),
    Form(HashMap<String, String>),
    Binary(Vec<u8>),
    None,
}

// Парсер для body
fn parse_body(body_str: &str) -> Result<BodyType, String> {
    if body_str.trim().is_empty() {
        return Ok(BodyType::None);
    }

    // Пытаемся парсить как JSON
    if let Ok(json_value) = serde_json::from_str::<Value>(body_str) {
        return Ok(BodyType::Json(json_value));
    }

    // Пытаемся парсить как Form данные (key=value&key2=value2)
    if body_str.contains('=') && !body_str.starts_with('{') && !body_str.starts_with('[') {
        let mut form_data = HashMap::new();
        for pair in body_str.split('&') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() == 2 {
                form_data.insert(parts[0].to_string(), parts[1].to_string());
            }
        }
        if !form_data.is_empty() {
            return Ok(BodyType::Form(form_data));
        }
    }

    // Пытаемся декодировать как base64
    if let Ok(decoded) = general_purpose::STANDARD.decode(body_str) {
        return Ok(BodyType::Binary(decoded));
    }

    // По умолчанию как текст
    Ok(BodyType::Text(body_str.to_string()))
}

// Конфигурация запроса
#[derive(Debug, Clone)]
struct RequestConfig {
    url: String,
    method: HttpMethod,
    body: BodyType,
    headers: HashMap<String, String>,
    timeout_secs: u64,
    content_type: Option<String>,
}

impl RequestConfig {
    fn from_cli(
        url: String,
        method: HttpMethod,
        body_str: Option<String>,
        headers: Vec<String>,
        timeout_secs: u64,
        content_type: Option<String>,
    ) -> Result<Self, String> {
        let body = if let Some(body_str) = body_str {
            parse_body(&body_str)?
        } else {
            BodyType::None
        };

        let mut headers_map = HashMap::new();
        for header in headers {
            let parts: Vec<&str> = header.splitn(2, ':').collect();
            if parts.len() == 2 {
                headers_map.insert(
                    parts[0].trim().to_string(),
                    parts[1].trim().to_string(),
                );
            } else {
                return Err(format!("Некорректный заголовок: {}", header));
            }
        }

        Ok(Self {
            url,
            method,
            body,
            headers: headers_map,
            timeout_secs,
            content_type,
        })
    }
}

// Конфигурация через CLI
#[derive(Parser)]
#[command(name = "Load Simulator")]
#[command(about = "Симулятор нагрузки с поддержкой различных HTTP методов", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Одновременные запросы от N пользователей
    Burst {
        /// Количество одновременных запросов
        #[arg(short, long, default_value_t = 20)]
        users: usize,
        
        /// URL бэкенда
        #[arg(short = 'U', long, default_value = "http://localhost:3000/api/test")]
        url: String,
        
        /// HTTP метод
        #[arg(short = 'X', long, value_enum, default_value = "post")]
        method: HttpMethod,
        
        /// Body запроса (JSON, form data, или текст)
        #[arg(short = 'd', long)]
        body: Option<String>,
        
        /// Заголовки в формате "Header: Value" (можно указать несколько)
        #[arg(short = 'H', long)]
        headers: Vec<String>,
        
        /// Content-Type (если не указан в headers)
        #[arg(short = 'c', long)]
        content_type: Option<String>,
        
        /// Максимальное время ожидания в секундах
        #[arg(short = 't', long, default_value_t = 30)]
        timeout: u64,
        
        /// Валидировать URL перед отправкой
        #[arg(long, default_value_t = true)]
        validate_url: bool,
    },
    
    /// Симуляция постоянной нагрузки (RPS)
    Rps {
        /// Запросов в секунду
        #[arg(short, long, default_value_t = 20)]
        rps: usize,
        
        /// Длительность теста в секундах
        #[arg(short, long, default_value_t = 10)]
        duration: u64,
        
        /// URL бэкенда
        #[arg(short = 'U', long, default_value = "http://localhost:3000/api/test")]
        url: String,
        
        /// HTTP метод
        #[arg(short = 'X', long, value_enum, default_value = "post")]
        method: HttpMethod,
        
        /// Body запроса (JSON, form data, или текст)
        #[arg(short = 'd', long)]
        body: Option<String>,
        
        /// Заголовки в формате "Header: Value" (можно указать несколько)
        #[arg(short = 'H', long)]
        headers: Vec<String>,
        
        /// Content-Type (если не указан в headers)
        #[arg(short = 'c', long)]
        content_type: Option<String>,
        
        /// Максимальное время ожидания в секундах
        #[arg(short = 't', long, default_value_t = 30)]
        timeout: u64,
        
        /// Валидировать URL перед отправкой
        #[arg(long, default_value_t = true)]
        validate_url: bool,
        
        /// Динамические параметры в body (например, {{userId}})
        #[arg(long, default_value_t = false)]
        dynamic_body: bool,
    },
    
    /// Проверка конфигурации запроса (без отправки)
    Check {
        /// URL бэкенда
        #[arg(short = 'U', long)]
        url: String,
        
        /// HTTP метод
        #[arg(short = 'X', long, value_enum, default_value = "post")]
        method: HttpMethod,
        
        /// Body запроса
        #[arg(short = 'd', long)]
        body: Option<String>,
        
        /// Заголовки в формате "Header: Value"
        #[arg(short = 'H', long)]
        headers: Vec<String>,
    },
    Multi(MultiUrlConfig)
}

// Результат запроса
#[derive(Debug)]
struct RequestResult {
    user_id: usize,
    success: bool,
    duration: Duration,
    status_code: Option<u16>,
    error: Option<String>,
    url: String,
    //method: String,
}

// Статистика теста
#[derive(Debug, Default)]
struct TestStats {
    total_requests: usize,
    successful: usize,
    failed: usize,
    min_duration: Duration,
    max_duration: Duration,
    total_duration: Duration,
    avg_duration: Duration,
    status_codes: HashMap<u16, usize>,
}

impl TestStats {
    fn new() -> Self {
        Self {
            min_duration: Duration::from_secs(u64::MAX),
            max_duration: Duration::from_secs(0),
            ..Default::default()
        }
    }
    
    fn add_result(&mut self, result: &RequestResult) {
        self.total_requests += 1;
        
        if result.success {
            self.successful += 1;
            
            if let Some(status) = result.status_code {
                *self.status_codes.entry(status).or_insert(0) += 1;
            }
            
            self.total_duration += result.duration;
            
            if result.duration < self.min_duration {
                self.min_duration = result.duration;
            }
            if result.duration > self.max_duration {
                self.max_duration = result.duration;
            }
        } else {
            self.failed += 1;
        }
    }
    
    fn calculate_final(&mut self) {
        if self.successful > 0 {
            self.avg_duration = self.total_duration / self.successful as u32;
        }
    }
    
    fn print_summary(&self) {
        println!("\n📊 Результаты теста:");
        println!("{}", "=".repeat(40));
        println!("Всего запросов: {}", self.total_requests);
        println!("Успешно: {}", self.successful);
        println!("Неудачно: {}", self.failed);
        
        if self.total_requests > 0 {
            println!("Успешность: {:.1}%", 
                (self.successful as f32 / self.total_requests as f32) * 100.0);
        }
        
        if !self.status_codes.is_empty() {
            println!("\n📈 Коды ответа:");
            let mut codes: Vec<_> = self.status_codes.iter().collect();
            codes.sort_by_key(|(code, _)| *code);
            for (code, count) in codes {
                println!("  {}: {} запросов", code, count);
            }
        }
        
        if self.successful > 0 {
            println!("\n⏱️  Время ответа:");
            println!("  Минимальное: {:.2}ms", self.min_duration.as_millis());
            println!("  Максимальное: {:.2}ms", self.max_duration.as_millis());
            println!("  Среднее: {:.2}ms", self.avg_duration.as_millis());
        }
    }
}

async fn make_request(
    client: &Client,
    config: &RequestConfig,
    user_id: usize,
    dynamic_body: bool,
) -> RequestResult {
    let start_time = Instant::now();
    let timestamp = Utc::now();
    let method_str = format!("{:?}", config.method).to_uppercase();
    
    // Подготавливаем body с динамическими значениями
    let body = if dynamic_body {
        prepare_dynamic_body(&config.body, user_id, timestamp)
    } else {
        config.body.clone()
    };
    
    // Создаем запрос
    let mut request_builder = client
        .request(config.method.clone().into(), &config.url)
        .timeout(Duration::from_secs(config.timeout_secs));
    
    // Добавляем заголовки
    for (key, value) in &config.headers {
        request_builder = request_builder.header(key, value);
    }
    
    // Добавляем Content-Type если указан
    if let Some(content_type) = &config.content_type {
        request_builder = request_builder.header("Content-Type", content_type);
    }
    
    // Добавляем body в зависимости от типа
    match body {
        BodyType::Json(json_value) => {
            request_builder = request_builder.json(&json_value);
        }
        BodyType::Text(text) => {
            request_builder = request_builder.body(text);
        }
        BodyType::Form(form_data) => {
            request_builder = request_builder.form(&form_data);
        }
        BodyType::Binary(data) => {
            request_builder = request_builder.body(data);
        }
        BodyType::None => {}
    }
    
    // Отправляем запрос
    match request_builder.send().await {
        Ok(response) => {
            let duration = start_time.elapsed();
            let status = response.status();
            let success = status.is_success();
            
            let status_symbol = if success { "✅" } else { "❌" };
            println!("👤 {} {} {} {} {:.2}ms", 
                user_id, method_str, config.url, status_symbol, duration.as_millis());
            
            RequestResult {
                user_id,
                success,
                duration,
                status_code: Some(status.as_u16()),
                error: if !success {
                    Some(format!("HTTP {}", status))
                } else {
                    None
                },
                url: config.url.clone(),
                //method: method_str,
            }
        }
        Err(e) => {
            let duration = start_time.elapsed();
            println!("👤 {} {} {} ❌ Ошибка: {} {:.2}ms", 
                user_id, method_str, config.url, e, duration.as_millis());
            
            RequestResult {
                user_id,
                success: false,
                duration,
                status_code: None,
                error: Some(e.to_string()),
                url: config.url.clone(),
                //method: method_str,
            }
        }
    }
}

fn prepare_dynamic_body(body: &BodyType, user_id: usize, timestamp: chrono::DateTime<Utc>) -> BodyType {
    match body {
        BodyType::Text(text) => {
            let replaced = text
                .replace("{{userId}}", &user_id.to_string())
                .replace("{{timestamp}}", &timestamp.to_rfc3339())
                .replace("{{uuid}}", &uuid::Uuid::new_v4().to_string());
            BodyType::Text(replaced)
        }
        BodyType::Json(json_value) => {
            let json_str = json_value.to_string();
            let replaced = json_str
                .replace("\"{{userId}}\"", &user_id.to_string())
                .replace("{{userId}}", &user_id.to_string())
                .replace("{{timestamp}}", &format!("\"{}\"", timestamp.to_rfc3339()))
                .replace("{{uuid}}", &format!("\"{}\"", uuid::Uuid::new_v4()));
            
            match serde_json::from_str::<Value>(&replaced) {
                Ok(new_json) => BodyType::Json(new_json),
                Err(_) => BodyType::Text(replaced),
            }
        }
        BodyType::Form(form_data) => {
            let mut new_form = HashMap::new();
            for (key, value) in form_data {
                let new_value = value
                    .replace("{{userId}}", &user_id.to_string())
                    .replace("{{timestamp}}", &timestamp.to_rfc3339());
                new_form.insert(key.clone(), new_value);
            }
            BodyType::Form(new_form)
        }
        other => other.clone(),
    }
}

fn validate_url(url: &str) -> Result<(), String> {
    Url::parse(url)
        .map_err(|e| format!("Некорректный URL: {}", e))
        .and_then(|parsed| {
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                Err("Поддерживаются только http и https схемы".to_string())
            } else {
                Ok(())
            }
        })
}

async fn simulate_burst(
    config: RequestConfig,
    users: usize,
    should_validate_url: bool,
    dynamic_body: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if should_validate_url {
        validate_url(&config.url)?;
    }
    
    println!("🚀 Запуск {} одновременных запросов", users);
    println!("🌐 Метод: {:?}", config.method);
    println!("🔗 URL: {}", config.url);
    println!("⏱️  Таймаут: {} секунд", config.timeout_secs);
    
    if !config.headers.is_empty() {
        println!("📋 Заголовки:");
        for (key, value) in &config.headers {
            println!("  {}: {}", key, value);
        }
    }
    
    match &config.body {
        BodyType::Json(json) => println!("📦 Body (JSON): {}", json),
        BodyType::Text(text) => println!("📦 Body (текст): {}", text),
        BodyType::Form(form) => println!("📦 Body (form): {:?}", form),
        BodyType::Binary(data) => println!("📦 Body (binary): {} байт", data.len()),
        BodyType::None => println!("📦 Body: нет"),
    }
    
    println!("{}", "=".repeat(50));
    
    let client = Client::new();
    let start_time = Instant::now();
    
    // Создаем задачи для всех пользователей
    let tasks: Vec<_> = (1..=users)
        .map(|user_id| {
            let client = client.clone();
            let config = config.clone();
            
            tokio::spawn(async move {
                make_request(&client, &config, user_id, dynamic_body).await
            })
        })
        .collect();
    
    // Ждем завершения всех задач
    let results = join_all(tasks).await;
    
    // Обрабатываем результаты
    let mut stats = TestStats::new();
    let mut all_results = Vec::new();
    
    for result in results {
        match result {
            Ok(request_result) => {
                stats.add_result(&request_result);
                all_results.push(request_result);
            }
            Err(e) => {
                eprintln!("Ошибка в задаче: {}", e);
            }
        }
    }
    
    stats.calculate_final();
    stats.print_summary();
    
    let total_duration = start_time.elapsed();
    println!("\n⏰ Общее время теста: {:.2} секунд", total_duration.as_secs_f32());
    
    // Детали по неудачным запросам
    if stats.failed > 0 {
        println!("\n🔍 Неудачные запросы (первые 5):");
        for result in all_results.iter().filter(|r| !r.success).take(5) {
            println!("  Пользователь {}: {}", result.user_id, result.error.as_deref().unwrap_or("Unknown"));
        }
    }
    
    Ok(())
}

async fn simulate_rps(
    config: RequestConfig,
    rps: usize,
    duration_secs: u64,
    should_validate_url: bool,
    dynamic_body: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if should_validate_url {
        validate_url(&config.url)?;
    }
    
    println!("📈 Симуляция {} RPS в течение {} секунд", rps, duration_secs);
    println!("🌐 Метод: {:?}", config.method);
    println!("🔗 URL: {}", config.url);
    println!("⏱️  Таймаут: {} секунд", config.timeout_secs);
    println!("{}", "=".repeat(50));
    
    let client = Client::new();
    let semaphore = Arc::new(Semaphore::new(rps * 2));
    
    let mut global_stats = TestStats::new();
    let mut total_requests = 0;
    
    let test_start = Instant::now();
    
    for second in 0..duration_secs {
        let second_start = Instant::now();
        let batch_start_user = total_requests + 1;
        
        println!("\n🕒 Секунда {}:", second + 1);
        
        // Создаем задачи для текущей секунды
        let mut batch_tasks = Vec::new();
        
        for i in 0..rps {
            let client = client.clone();
            let config = config.clone();
            let semaphore = semaphore.clone();
            let user_id = batch_start_user + i;
            
            let task = tokio::spawn(async move {
                let _permit = semaphore.acquire().await.expect("Semaphore error");
                make_request(&client, &config, user_id, dynamic_body).await
            });
            
            batch_tasks.push(task);
            total_requests += 1;
        }
        
        // Ждем завершения всех задач в этой секунде
        let batch_results = join_all(batch_tasks).await;
        
        // Собираем статистику по батчу
        let mut batch_successful = 0;
        let mut batch_duration_total = Duration::ZERO;
        
        for result in batch_results {
            match result {
                Ok(request_result) => {
                    global_stats.add_result(&request_result);
                    if request_result.success {
                        batch_successful += 1;
                        batch_duration_total += request_result.duration;
                    }
                }
                Err(e) => {
                    eprintln!("Ошибка в задаче: {}", e);
                    global_stats.failed += 1;
                }
            }
        }
        
        // Выводим статистику за секунду
        println!("  Запросов: {}/{} успешно", batch_successful, rps);
        if batch_successful > 0 {
            let avg_duration = batch_duration_total / batch_successful as u32;
            println!("  Среднее время: {:.2}ms", avg_duration.as_millis());
        }
        
        // Ждем до конца секунды, если задачи выполнились быстрее
        let elapsed = second_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            let sleep_time = Duration::from_secs(1) - elapsed;
            tokio::time::sleep(sleep_time).await;
        }
    }
    
    global_stats.calculate_final();
    
    println!("\n{}", "=".repeat(50));
    println!("🎯 ИТОГИ ТЕСТА:");
    global_stats.print_summary();
    
    let total_test_duration = test_start.elapsed();
    println!("\n⏰ Общее время теста: {:.2} секунд", 
        total_test_duration.as_secs_f32());
    
    let actual_rps = total_requests as f32 / duration_secs as f32;
    println!("📊 Фактический RPS: {:.1}", actual_rps);
    
    Ok(())
}

fn check_config(
    url: String,
    method: HttpMethod,
    body_str: Option<String>,
    headers: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 Проверка конфигурации запроса:");
    println!("{}", "=".repeat(40));
    
    // Валидация URL
    match validate_url(&url) {
        Ok(_) => println!("✅ URL: {}", url),
        Err(e) => println!("❌ URL: {} - {}", url, e),
    }
    
    println!("✅ Метод: {:?}", method);
    
    // Парсинг и валидация body
    if let Some(body_str) = body_str {
        match parse_body(&body_str) {
            Ok(body_type) => {
                println!("✅ Body распознан как:");
                match body_type {
                    BodyType::Json(json) => println!("   JSON: {}", json),
                    BodyType::Text(text) => println!("   Текст ({} символов)", text.len()),
                    BodyType::Form(form) => {
                        println!("   Form данные:");
                        for (key, value) in form {
                            println!("     {} = {}", key, value);
                        }
                    }
                    BodyType::Binary(data) => println!("   Бинарные данные ({} байт)", data.len()),
                    BodyType::None => println!("   Нет body"),
                }
            }
            Err(e) => println!("❌ Ошибка парсинга body: {}", e),
        }
    } else {
        println!("✅ Body: не указан");
    }
    
    // Валидация заголовков
    if !headers.is_empty() {
        println!("📋 Заголовки:");
        for header in headers {
            let parts: Vec<&str> = header.splitn(2, ':').collect();
            if parts.len() == 2 {
                println!("   ✅ {}: {}", parts[0].trim(), parts[1].trim());
            } else {
                println!("   ❌ Некорректный формат: {}", header);
            }
        }
    }
    
    println!("\n💡 Примеры использования:");
    println!("  burst -U https://api.example.com/users -X GET");
    println!("  burst -U https://api.example.com/users -X POST -d '{{\"name\":\"John\"}}'");
    println!("  burst -U https://api.example.com/login -X POST -d 'username=admin&password=123'");
    println!("  burst -U https://api.example.com/upload -X PUT -d 'SGVsbG8gV29ybGQ=' -H 'Authorization: Bearer token'");
    
    Ok(())
}

async fn simulate_multiple_urls(
    tester: Arc<MultiUrlTester>,
    users: usize,
    should_validate_url: bool,
    dynamic_body: bool,
) -> Result<TestStats, Box<dyn std::error::Error>> {
    println!("🚀 Запуск {} запросов на {} URL", users, tester.configs.len());
    
    // Валидация всех URL
    if should_validate_url {
        for config in &tester.configs {
            validate_url(&config.url)?;
        }
    }
    
    // Вывод информации о URL
    println!("\n📋 Тестируемые URL:");
    for (i, config) in tester.configs.iter().enumerate() {
        println!("  {}: {} (метод: {:?})", i + 1, config.url, config.method);
    }
    
    println!("📊 Распределение запросов: {:?}", tester.distribution);
    println!("{}", "=".repeat(50));
    
    let client = Client::new();
    let start_time = Instant::now();
    
    // Создаем задачи для всех пользователей
    let tasks: Vec<_> = (1..=users)
        .map(|user_id| {
            let client = client.clone();
            let tester = tester.clone();
            
            tokio::spawn(async move {
                let config = tester.get_next_config(user_id);
                make_request(&client, config, user_id, dynamic_body).await
            })
        })
        .collect();
    
    // Ждем завершения всех задач
    let results = join_all(tasks).await;
    
    // Обрабатываем результаты
    let mut stats = TestStats::new();
    let mut all_results = Vec::new();
    
    for result in results {
        match result {
            Ok(request_result) => {
                stats.add_result(&request_result);
                all_results.push(request_result);
            }
            Err(e) => {
                eprintln!("Ошибка в задаче: {}", e);
            }
        }
    }
    
    stats.calculate_final();
    
    // Выводим сводную статистику
    println!("\n{}", "=".repeat(50));
    println!("📊 СВОДНАЯ СТАТИСТИКА:");
    stats.print_summary();
    
    // Детальная статистика по каждому URL
    println!("\n📈 Статистика по URL:");
    println!("{}", "-".repeat(40));
    
    let mut url_stats: HashMap<String, (usize, usize, Duration)> = HashMap::new(); // (успешно, всего, суммарное время)
    
    for result in &all_results {
        let entry = url_stats.entry(result.url.clone()).or_insert((0, 0, Duration::ZERO));
        entry.1 += 1; // всего запросов
        if result.success {
            entry.0 += 1; // успешных
            entry.2 += result.duration; // суммарное время
        }
    }
    
    for (url, (successful, total, total_duration)) in url_stats {
        let success_rate = if total > 0 {
            (successful as f32 / total as f32) * 100.0
        } else {
            0.0
        };
        
        let avg_duration = if successful > 0 {
            total_duration / successful as u32
        } else {
            Duration::ZERO
        };
        
        println!("🔗 {}", url);
        println!("   Запросов: {}/{} успешно ({:.1}%)", successful, total, success_rate);
        if successful > 0 {
            println!("   Среднее время: {:.2}ms", avg_duration.as_millis());
        }
        println!();
    }
    
    let total_duration = start_time.elapsed();
    println!("⏰ Общее время теста: {:.2} секунд", total_duration.as_secs_f32());
    
    // Детали по неудачным запросам - теперь у нас есть URL в результатах
    if stats.failed > 0 {
        println!("\n🔍 Неудачные запросы (первые 10):");
        let failed_results: Vec<_> = all_results.iter()
            .filter(|r| !r.success)
            .take(10)
            .collect();
        
        for result in failed_results {
            println!("  Пользователь {} ({}): {}", 
                result.user_id, result.url, result.error.as_deref().unwrap_or("Unknown"));
        }
    }
    
    Ok(stats)
}

fn create_configs_from_urls(
    urls: Vec<String>,
    method: HttpMethod,
    body_str: Option<String>,
    headers: Vec<String>,
    timeout: u64,
    content_type: Option<String>,
) -> Result<Vec<RequestConfig>, String> {
    let mut configs = Vec::new();
    
    for url in urls {
        let config = RequestConfig::from_cli(
            url,
            method.clone(),
            body_str.clone(),
            headers.clone(),
            timeout,
            content_type.clone(),
        )?;
        configs.push(config);
    }
    
    Ok(configs)
}

fn load_configs_from_file(
    file_path: &str,
    common_headers: Vec<String>,
    common_timeout: u64,
) -> Result<Vec<RequestConfig>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(file_path)?;
    //let extension = file_path.split('.').last().unwrap_or("").to_lowercase();
    
    // Здесь можно добавить парсинг JSON/YAML/TOML
    // Для простоты будем считать, что файл содержит URL по одному на строку
    let urls: Vec<String> = content.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_string())
        .collect();
    
    if urls.is_empty() {
        return Err("No URLs found in config file".into());
    }
    
    // Создаем конфигурации для каждого URL
    let configs = urls.into_iter()
        .map(|url| {
            RequestConfig::from_cli(
                url,
                HttpMethod::GET, // По умолчанию GET
                None,
                common_headers.clone(),
                common_timeout,
                None,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    
    Ok(configs)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    
    match cli.command {
        Commands::Burst { 
            users, 
            url, 
            method, 
            body, 
            headers, 
            content_type,
            timeout, 
            validate_url: should_validate_url,
        } => {
            let config = RequestConfig::from_cli(
                url, method, body, headers, timeout, content_type
            )?;
            
            simulate_burst(config, users, should_validate_url, false).await?;
        }
        Commands::Rps { 
            rps, 
            duration, 
            url, 
            method, 
            body, 
            headers,
            content_type,
            timeout, 
            validate_url: should_validate_url,
            dynamic_body,
        } => {
            let config = RequestConfig::from_cli(
                url, method, body, headers, timeout, content_type
            )?;
            
            simulate_rps(config, rps, duration, should_validate_url, dynamic_body).await?;
        }
        Commands::Check { 
            url, 
            method, 
            body, 
            headers,
        } => {
            check_config(url, method, body, headers)?;
        }
        Commands::Multi(multi_config) => {
            handle_multi_command(multi_config).await?;
        }
    }
    
    Ok(())
}

async fn handle_multi_command(config: MultiUrlConfig) -> Result<(), Box<dyn std::error::Error>> {
    let configs = if let Some(file_path) = &config.config_file {
        // Загружаем из файла
        load_configs_from_file(file_path, config.headers.clone(), config.timeout)?
    } else if let Some(url_list) = &config.url_list {
        // Используем список URL из CLI
        create_configs_from_urls(
            url_list.clone(),
            config.method,
            config.body.clone(),
            config.headers,
            config.timeout,
            config.content_type.clone(),
        )?
    } else {
        return Err("Either --config-file or --url-list must be specified".into());
    };
    
    if configs.is_empty() {
        return Err("No URLs configured for testing".into());
    }
    
    // Создаем тестер
    let tester = MultiUrlTester::new(configs, config.distribution.clone());
    let tester_arc = Arc::new(tester);
    
    // Для multi режима используем burst логику, но можно добавить RPS
    // Определяем количество пользователей (можно добавить параметр)
    let users = 20; // По умолчанию
    
    println!("🎯 ЗАПУСК МУЛЬТИ-URL ТЕСТА");
    println!("{}", "=".repeat(50));
    
    let stats = simulate_multiple_urls(
        tester_arc,
        users,
        config.validate_url,
        false, // dynamic_body - можно добавить в конфиг
    ).await?;
    
    // Выводим дополнительные метрики
    println!("\n🎯 ИТОГОВЫЕ МЕТРИКИ:");
    println!("📈 Общая пропускная способность: {:.1} запр/сек", 
        stats.total_requests as f32 / stats.total_duration.as_secs_f32());
    
    Ok(())
}