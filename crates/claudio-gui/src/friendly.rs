//! Plain-language texts for people without technical background.

use claudio_core::permissions::Risk;

/// Icon and short description of what a tool does ("Lendo um arquivo").
pub fn tool(name: &str) -> (&'static str, &'static str) {
    match name {
        "read_file" => ("📄", "Lendo um arquivo"),
        "write_file" => ("📝", "Criando um arquivo"),
        "edit_file" => ("✏", "Editando um arquivo"),
        "list_dir" | "glob" => ("📁", "Olhando uma pasta"),
        "grep" => ("🔍", "Procurando dentro dos arquivos"),
        "move_path" => ("📦", "Movendo ou renomeando"),
        "delete_path" => ("🗑", "Mandando para a lixeira"),
        "shell" => ("⚙", "Executando um comando"),
        "job_status" => ("⏱", "Acompanhando uma tarefa"),
        "system_info" | "process_list" => ("💻", "Verificando o computador"),
        "web_search" => ("🌐", "Pesquisando na internet"),
        "web_fetch" => ("🌐", "Abrindo uma página da internet"),
        "browser" => ("🌐", "Usando o navegador"),
        "doc_read" => ("📄", "Lendo um documento"),
        "doc_create" => ("📝", "Criando um documento"),
        "doc_convert" => ("🔄", "Convertendo um documento"),
        "latex_compile" => ("📐", "Gerando um PDF"),
        "image_edit" => ("🖼", "Editando uma imagem"),
        "ocr" => ("🔤", "Lendo o texto de uma imagem"),
        "media_info" => ("🎞", "Analisando um áudio ou vídeo"),
        "media_convert" => ("🎞", "Convertendo um áudio ou vídeo"),
        "transcribe" => ("🎙", "Transcrevendo um áudio"),
        "speak" => ("🔊", "Falando em voz alta"),
        "memory_save" => ("🧠", "Guardando uma lembrança"),
        "memory_search" | "history_search" => ("🧠", "Lembrando de conversas anteriores"),
        "update_plan" => ("🗒", "Organizando os passos"),
        "delegate_task" => ("👥", "Pedindo ajuda a um assistente"),
        "skill_load" => ("🧩", "Carregando uma habilidade"),
        _ => ("🧩", "Usando uma ferramenta"),
    }
}

/// The question shown when the assistant needs permission.
pub fn approval_title(tool: &str, risk: Risk) -> &'static str {
    match (tool, risk) {
        ("delete_path", _) => "O Claudio quer mandar algo para a lixeira",
        ("write_file" | "edit_file" | "doc_create" | "doc_convert", _) => {
            "O Claudio quer criar ou alterar um arquivo"
        }
        ("move_path", _) => "O Claudio quer mover ou renomear algo",
        (_, Risk::Admin) => "O Claudio quer fazer uma mudança no sistema",
        (_, Risk::Destructive) => "O Claudio quer fazer algo que pode apagar dados",
        ("shell", _) | (_, Risk::Execute) => "O Claudio quer executar um comando",
        (_, Risk::Network) => "O Claudio quer acessar a internet",
        (_, Risk::Write) => "O Claudio quer salvar alterações",
        _ => "O Claudio precisa da sua permissão",
    }
}

/// One sentence explaining the risk in everyday words.
pub fn risk_hint(risk: Risk) -> &'static str {
    match risk {
        Risk::Read => "Ele só vai ler; nada será alterado.",
        Risk::Network => "Ele vai se conectar à internet.",
        Risk::Write => "Arquivos podem ser criados ou alterados.",
        Risk::Execute => "Um programa vai rodar no seu computador.",
        Risk::Destructive => "Isto pode apagar ou sobrescrever dados. Confira com atenção.",
        Risk::Admin => "Isto mexe no sistema. Só permita se tiver certeza.",
    }
}

/// Short ideas shown on the empty chat.
pub const SUGGESTIONS: &[(&str, &str)] = &[
    ("📝", "Escreva um currículo simples e salve como PDF"),
    ("📊", "Crie uma planilha para controlar meus gastos do mês"),
    (
        "🌐",
        "Pesquise as notícias de hoje sobre tecnologia e faça um resumo",
    ),
    (
        "📁",
        "Organize os arquivos da minha pasta de trabalho por tipo",
    ),
    ("🖼", "Reduza o tamanho das imagens da pasta de trabalho"),
    ("💻", "Verifique se meu computador está com pouco espaço"),
];
