unit Unit1;

{$mode objfpc}{$H+}

// The player window: Open a file, then Play, Pause and Stop. Video is drawn in VideoPanel.

interface

uses
  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls, ExtCtrls, PascodaVlc;

type
  TForm1 = class(TForm)
    VideoPanel: TPanel;
    OpenButton: TButton;
    PlayButton: TButton;
    PauseButton: TButton;
    StopButton: TButton;
    StatusLabel: TLabel;
    OpenDialog1: TOpenDialog;
    Player: TPascodaVlcPlayer;
    procedure FormCreate(Sender: TObject);
    procedure OpenButtonClick(Sender: TObject);
    procedure PlayButtonClick(Sender: TObject);
    procedure PauseButtonClick(Sender: TObject);
    procedure StopButtonClick(Sender: TObject);
  private
    procedure ShowError(const Message: string);
  public
  end;

var
  Form1: TForm1;

implementation

{$R *.lfm}

procedure TForm1.FormCreate(Sender: TObject);
begin
  Player.VideoPanel := VideoPanel;
  StatusLabel.Caption := 'Open a file to start';
end;

procedure TForm1.ShowError(const Message: string);
begin
  StatusLabel.Caption := Message;
  ShowMessage(Message);
end;

procedure TForm1.OpenButtonClick(Sender: TObject);
begin
  if not OpenDialog1.Execute then
    Exit;
  try
    Player.Open(OpenDialog1.FileName);
    StatusLabel.Caption := ExtractFileName(OpenDialog1.FileName);
    Player.Play;
  except
    on E: EPascodaVlc do
      ShowError(E.Message);
  end;
end;

procedure TForm1.PlayButtonClick(Sender: TObject);
begin
  try
    Player.Play;
  except
    on E: EPascodaVlc do
      ShowError(E.Message);
  end;
end;

procedure TForm1.PauseButtonClick(Sender: TObject);
begin
  Player.Pause;
end;

procedure TForm1.StopButtonClick(Sender: TObject);
begin
  Player.Stop;
end;

initialization
  // The form loader creates the player by its class name, so the class must be registered.
  RegisterClass(TPascodaVlcPlayer);

end.
