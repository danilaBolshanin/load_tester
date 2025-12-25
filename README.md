# Общая справка
.\load_test.exe --help

# Справка по burst
.\load_test.exe burst --help

# Справка по rps
.\load_test.exe rps --help

# Справка по multi
.\load_test.exe multi --help

# Справка по check
.\load_test.exe check --help

 Простой GET запрос
.\load_test.exe burst -U "https://httpbin.org/get" -X GET -u 10

# POST с JSON
.\load_test.exe burst `
  -U "https://httpbin.org/post" `
  -X POST `
  -d '{"name": "John", "age": 30}' `
  -H "Content-Type: application/json" `
  -u 20

# PUT с form data
.\load_test.exe burst `
  -U "https://httpbin.org/put" `
  -X PUT `
  -d "username=admin&password=secret" `
  -u 15

# DELETE запрос с заголовками
.\load_test.exe burst `
  -U "https://httpbin.org/delete" `
  -X DELETE `
  -H "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..." `
  -H "X-Request-ID: 12345" `
  -u 5

# PATCH запрос
.\load_test.exe burst `
  -U "https://httpbin.org/patch" `
  -X PATCH `
  -d '{"status": "updated"}' `
  -u 10

# 10 запросов в секунду в течение 30 секунд
.\load_test.exe rps `
  -U "https://httpbin.org/get" `
  -r 10 `
  -d 30

# Тест нескольких URL через запятую
.\load_test.exe multi `
  --url-list "https://httpbin.org/get,https://httpbin.org/post,https://httpbin.org/put" `
  -u 30