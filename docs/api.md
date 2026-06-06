# MQTT Broker Management API

## Base URL

```
http://localhost:8080
```

## Authentication

### Get Token

```http
POST /api/auth/token
Content-Type: application/json

{
    "username": "admin",
    "password": "admin"
}
```

Response:
```json
{
    "success": true,
    "data": {
        "token": "eyJ...",
        "expires_in": 3600
    },
    "error": null
}
```

## Endpoints

### Health Check

```http
GET /health
```

Response:
```json
{
    "success": true,
    "data": {
        "status": "ok",
        "uptime_secs": 86400,
        "components": {
            "mqtt": { "status": "ok", "connections": 42 },
            "api": { "status": "ok" }
        }
    },
    "error": null
}
```

### List Connected Clients

```http
GET /api/clients
Authorization: Bearer <token>
```

Response:
```json
{
    "success": true,
    "data": [
        {
            "client_id": "device_01",
            "addr": "192.168.1.100:54321",
            "protocol_version": "V311",
            "connected_at_secs": 3600,
            "clean_session": true,
            "keep_alive": 60,
            "username": null
        }
    ],
    "error": null
}
```

### Get Client Details

```http
GET /api/clients/{client_id}
Authorization: Bearer <token>
```

### List All Subscribed Topics

```http
GET /api/subscriptions
Authorization: Bearer <token>
```

### Get Topic Subscribers

```http
GET /api/subscriptions/{topic}
Authorization: Bearer <token>
```

## Error Responses

```json
{
    "success": false,
    "data": null,
    "error": {
        "code": "UNAUTHORIZED",
        "message": "Invalid or expired token"
    }
}
```

## Error Codes

| Code | HTTP Status | Description |
|------|-------------|-------------|
| UNAUTHORIZED | 401 | Invalid or expired token |
| NOT_FOUND | 404 | Resource not found |
| TOKEN_ERROR | 500 | Token generation failure |
