# hull-terminal の統合スクリプト。
#
# デーモンが `bash --rcfile <このファイル> -i` で読み込ませる。ふだんの設定を読んだあと、
# コマンドの区切りをデーモンに知らせる合図 (OSC 133 / OSC 7) をプロンプトに仕込む。
#
#   OSC 133;A   プロンプトの始まり (k=c が付けば、続きの入力を待つ PS2)
#   OSC 133;B   プロンプトの終わり (ここからコマンドの入力)
#   OSC 133;C   コマンドの実行の始まり
#   OSC 133;D   コマンドの終わりと終了コード
#   OSC 7       今のディレクトリ
#   OSC 7133    git ブランチ (hull-terminal 独自)

# `--rcfile` を付けると bash はどちらも読まないので、ここで読む。
if [[ -r /etc/bash.bashrc ]]; then
  . /etc/bash.bashrc
fi
if [[ -r ~/.bashrc ]]; then
  . ~/.bashrc
fi

# 合図には起動ごとの合言葉を付ける。コマンドの出力に OSC 133 が混ざっていても、
# 合言葉が合わないのでデーモンは区切りと見なさない。子プロセスには渡さない。
if [[ $- == *i* && -n ${HULL_TERMINAL_NONCE-} && -z ${__hull_nonce-} ]]; then
  __hull_nonce=$HULL_TERMINAL_NONCE

  __hull_a=$'\e]133;A;hull='$__hull_nonce$'\a'
  __hull_k=$'\e]133;A;k=c;hull='$__hull_nonce$'\a'
  __hull_b=$'\e]133;B;hull='$__hull_nonce$'\a'
  __hull_c=$'\e]133;C;hull='$__hull_nonce$'\a'

  # $1 を URL の形 (%XX) にして __hull_encoded に入れる。
  __hull_encode() {
    local LC_ALL=C text=$1 out= char i
    for ((i = 0; i < ${#text}; i++)); do
      char=${text:i:1}
      case $char in
        [a-zA-Z0-9/._~-]) out+=$char ;;
        *)
          printf -v char '%%%02X' "'$char"
          out+=$char
          ;;
      esac
    done
    __hull_encoded=$out
  }

  # プロンプトの設定を書き換えられても、合図を入れ直す。
  __hull_wrap() {
    [[ $PS1 == *"$__hull_a"* ]] || PS1="\[$__hull_a\]$PS1\[$__hull_b\]"
    [[ $PS2 == *"$__hull_k"* ]] || PS2="\[$__hull_k\]$PS2"
    [[ $PS0 == *"$__hull_c"* ]] || PS0="$__hull_c$PS0"
  }

  # PROMPT_COMMAND の先頭で動く。直前のコマンドの終了コードを知らせ、同じ値を返すので、
  # あとに続く処理から見える $? は変わらない。
  __hull_precmd() {
    local code=$?
    printf '\e]133;D;%s;hull=%s\a' "$code" "$__hull_nonce"
    return "$code"
  }

  # PROMPT_COMMAND の最後で動く。
  __hull_postcmd() {
    local code=$? branch=
    __hull_encode "$PWD"
    printf '\e]7;file://%s%s\a' "$HOSTNAME" "$__hull_encoded"
    if [[ -n $__hull_git ]]; then
      branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null \
        || git rev-parse --quiet --short HEAD 2>/dev/null)
    fi
    __hull_encode "$branch"
    printf '\e]7133;hull=%s;branch=%s\a' "$__hull_nonce" "$__hull_encoded"
    __hull_wrap
    return "$code"
  }

  __hull_git=$(command -v git 2>/dev/null)

  # 区切りを改行にして、元の値が `;` で終わっていても壊れないようにする。
  if [[ ${PROMPT_COMMAND@a} == *a* ]]; then
    PROMPT_COMMAND=(__hull_precmd "${PROMPT_COMMAND[@]}" __hull_postcmd)
  else
    PROMPT_COMMAND="__hull_precmd${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}"$'\n__hull_postcmd'
  fi
  __hull_wrap

  # 入力欄のコマンドは括弧付き貼り付けで届く。~/.inputrc で切ってあっても有効にする。
  bind 'set enable-bracketed-paste on' 2>/dev/null
fi
unset HULL_TERMINAL_NONCE
