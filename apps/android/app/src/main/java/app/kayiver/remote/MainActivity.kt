package app.kayiver.remote

import android.content.Context
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/**
 * Kayıver Remote: durum + ortak monitör kumandası.
 * Mac'te `kayiver remote enable` ile açılan token korumalı LAN API'ye bağlanır.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { MaterialTheme(colorScheme = darkColorScheme()) { RemoteScreen() } }
    }
}

data class Settings(val host: String, val port: String, val token: String)

fun loadSettings(ctx: Context): Settings {
    val p = ctx.getSharedPreferences("kayiver", Context.MODE_PRIVATE)
    return Settings(
        p.getString("host", "") ?: "",
        p.getString("port", "24819") ?: "24819",
        p.getString("token", "") ?: "",
    )
}

fun saveSettings(ctx: Context, s: Settings) {
    ctx.getSharedPreferences("kayiver", Context.MODE_PRIVATE).edit()
        .putString("host", s.host).putString("port", s.port).putString("token", s.token).apply()
}

suspend fun api(s: Settings, method: String, path: String, body: String? = null): String =
    withContext(Dispatchers.IO) {
        val conn = URL("http://${s.host}:${s.port}$path").openConnection() as HttpURLConnection
        conn.requestMethod = method
        conn.connectTimeout = 3000
        conn.readTimeout = 3000
        conn.setRequestProperty("Authorization", "Bearer ${s.token}")
        if (body != null) {
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/json")
            conn.outputStream.use { it.write(body.toByteArray()) }
        }
        val text = (if (conn.responseCode in 200..299) conn.inputStream else conn.errorStream)
            ?.bufferedReader()?.readText() ?: ""
        if (conn.responseCode !in 200..299) throw RuntimeException("HTTP ${conn.responseCode}: $text")
        text
    }

@Composable
fun RemoteScreen() {
    val ctx = androidx.compose.ui.platform.LocalContext.current
    var settings by remember { mutableStateOf(loadSettings(ctx)) }
    var editing by remember { mutableStateOf(settings.host.isEmpty()) }
    val scope = rememberCoroutineScope()
    var status by remember { mutableStateOf<JSONObject?>(null) }
    var machines by remember { mutableStateOf(listOf<String>()) }
    var error by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(settings, editing) {
        if (editing) return@LaunchedEffect
        while (true) {
            try {
                if (machines.isEmpty()) {
                    val st = JSONObject(api(settings, "GET", "/api/state"))
                    val arr = st.getJSONArray("machines")
                    machines = (0 until arr.length()).map { arr.getJSONObject(it).getString("name") }
                }
                status = JSONObject(api(settings, "GET", "/api/status"))
                error = null
            } catch (e: Exception) {
                error = e.message
            }
            delay(2000)
        }
    }

    Column(
        Modifier.fillMaxSize().padding(24.dp).verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(16.dp)
    ) {
        Spacer(Modifier.height(24.dp))
        Text("kayıver", fontSize = 32.sp, fontWeight = FontWeight.ExtraBold, color = Color(0xFF34D399))
        Text("karşıya kayıver — uzaktan kumanda", color = Color.Gray)

        if (editing) {
            OutlinedTextField(settings.host, { settings = settings.copy(host = it) },
                label = { Text("Ana makine IP") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(settings.port, { settings = settings.copy(port = it) },
                label = { Text("Port") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(settings.token, { settings = settings.copy(token = it) },
                label = { Text("Token (kayiver remote enable)") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            Button(onClick = { saveSettings(ctx, settings); machines = listOf(); editing = false }) {
                Text("Bağlan")
            }
            return@Column
        }

        val st = status
        val shared = st?.optJSONObject("shared")
        val running = st?.optBoolean("running") == true

        Card(Modifier.fillMaxWidth()) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(if (running) "● çalışıyor" else "○ ulaşılamıyor",
                    color = if (running) Color(0xFF34D399) else Color(0xFFF87171))
                st?.optString("focus")?.takeIf { it.isNotEmpty() && it != "null" }?.let {
                    Text("imleç: $it", color = Color.Gray)
                }
                error?.let { Text(it, color = Color(0xFFF87171), fontSize = 12.sp) }
            }
        }

        if (shared != null && shared.optBoolean("configured")) {
            Text("Ortak monitör", fontWeight = FontWeight.Bold)
            val owner = shared.optString("owner")
            Text(if (owner.isNotEmpty()) "şu an: $owner" else "durum bilinmiyor", color = Color.Gray)
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                machines.forEach { name ->
                    val active = name == owner
                    Button(
                        onClick = {
                            // fire & forget; sonraki poll durumu tazeler
                            scope.launch {
                                try { api(settings, "POST", "/api/shared", "{\"owner\":\"$name\"}") }
                                catch (e: Exception) { error = e.message }
                            }
                        },
                        colors = if (active) ButtonDefaults.buttonColors(containerColor = Color(0xFF34D399))
                                 else ButtonDefaults.outlinedButtonColors(),
                        modifier = Modifier.weight(1f).height(64.dp)
                    ) { Text(name) }
                }
            }
            shared.optString("error").takeIf { it.isNotEmpty() && it != "null" }?.let {
                Text(it, color = Color(0xFFFBBF24), fontSize = 12.sp)
            }
        } else {
            Text("Ortak monitör tanımlı değil — Mac'teki editörden ayarla.", color = Color.Gray)
        }

        Spacer(Modifier.weight(1f))
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            TextButton(onClick = { editing = true }) { Text("ayarlar") }
        }
    }
}
